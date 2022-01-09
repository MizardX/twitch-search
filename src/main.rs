use std::process::exit;
use std::{cmp, env};

use chrono::prelude::*;
use clap::Parser;
use serde_json::Value;

const ROOT_URL: &str = "https://api.twitch.tv/helix/streams?first=100&game_id=1469308723";

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// Term to search for
    #[clap(default_value = "")]
    term: String,

    /// Streamers to exclude
    #[clap(short = 'x', long)]
    exclude: Option<Vec<String>>,

    /// Only show langauge (en, fr, ...)
    #[clap(short = 'l', long)]
    lang: Option<String>,

    /// Search on word boundary
    #[clap(short, long)]
    word: bool,
}

macro_rules! to_str {
    ($val: expr, $key: expr) => {
        $val.get($key).unwrap().as_str().unwrap().to_string()
    };
}

macro_rules! to_num {
    ($val: expr, $key: expr) => {
        $val.get($key).unwrap().as_i64().unwrap()
    };
}

fn to_instant(ds: &str) -> String {
    match ds.parse::<DateTime<Utc>>() {
        Ok(val) => {
            let dur = Utc::now() - val;
            format!("{:02}:{:02}", dur.num_hours(), dur.num_minutes() % 60)
        }
        Err(_e) => "".to_string(),
    }
}

#[derive(Debug)]
struct Entry {
    lang: String,
    display_name: String,
    title: String,
    viewer_count: i64,
    live_duration: String,
}

fn filter(
    entry: &Entry,
    whole_word: bool,
    term: &str,
    ignored_names: &[String],
    lang: &Option<String>,
) -> bool {
    if ignored_names.contains(&entry.display_name.to_lowercase()) {
        return false;
    }

    if let Some(lang) = lang {
        if &entry.lang != lang {
            return false;
        }
    }

    if whole_word {
        for e in entry
            .title
            .to_lowercase()
            .split(|c: char| !c.is_alphabetic())
        {
            if e == term {
                return true;
            }
        }
        return false;
    }

    entry.title.to_lowercase().contains(term)
}

// -----------------------------------------------------------------------------
//     - Table formatting -
// -----------------------------------------------------------------------------
#[allow(unused)]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug)]
struct Table<const N: usize> {
    widths: [usize; N],
    rows: Vec<[String; N]>,
    align: [Align; N],
}

impl<const N: usize> Table<N> {
    fn new() -> Self {
        Table {
            widths: [0; N],
            rows: Vec::new(),
            align: [Align::Left; N],
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn set_align(&mut self, column: usize, align: Align) {
        self.align[column] = align;
    }

    fn push(&mut self, row: [String; N]) {
        for (i, cell) in row.iter().enumerate().take(N - 1) {
            self.widths[i] = cmp::max(self.widths[i], cell.len());
        }
        self.rows.push(row);
    }

    fn print(&self) {
        for row in &self.rows {
            #[allow(clippy::needless_range_loop)]
            for i in 0..N - 1 {
                match self.align[i] {
                    Align::Left => print!("{0:<1$} | ", row[i], self.widths[i]),
                    Align::Center => print!("{0:^1$} | ", row[i], self.widths[i]),
                    Align::Right => print!("{0:>1$} | ", row[i], self.widths[i]),
                }
            }
            println!("{}", row[N - 1]); // last column always left aligned
        }
    }
}

fn to_entry(value: &Value) -> Entry {
    Entry {
        lang: to_str!(value, "language"),
        display_name: to_str!(value, "user_name"),
        title: to_str!(value, "title"),
        viewer_count: to_num!(value, "viewer_count"),
        live_duration: to_instant(&to_str!(value, "started_at")),
    }
}

fn format_row(entry: Entry) -> [String; 5] {
    [
        entry.lang,
        format!("https://twitch.tv/{}", entry.display_name),
        format!("{} viewers", entry.viewer_count),
        entry.live_duration,
        entry.title.replace(|c: char| c.is_control(), " "),
    ]
}

fn configure_agent() -> ureq::Agent {
    // -----------------------------------------------------------------------------
    //     - Proxy -
    // -----------------------------------------------------------------------------
    let proxy = env::var("https_proxy")
        .ok()
        .and_then(|p| ureq::Proxy::new(p).ok());

    let mut agent = ureq::AgentBuilder::new();
    if let Some(proxy) = proxy {
        agent = agent.proxy(proxy);
    }

    agent.build()
}

fn aquire_access_token() -> String {
    let agent = configure_agent();

    // -----------------------------------------------------------------------------
    //     - Token -
    // -----------------------------------------------------------------------------
    let client_id = env::var("TWITCH_CLIENT_ID").unwrap_or_else(|_| {
        eprintln!("Client id missing. Please set the TWITCH_CLIENT_ID environment variable.");
        exit(1);
    });

    let client_secret = env::var("TWITCH_CLIENT_SECRET").unwrap_or_else(|_| {
        eprintln!(
            "Client secret missing. Please set the TWITCH_CLIENT_SECRET environment variable."
        );
        exit(1);
    });

    let resp = agent
        .post("https://id.twitch.tv/oauth2/token")
        .send_form(&[
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("grant_type", "client_credentials"),
        ])
        .unwrap_or_else(|e| {
            eprintln!("Failed to get acccess token: {:?}", e);
            exit(1);
        });

    let json = resp.into_json::<Value>().unwrap_or_else(|e| {
        eprintln!("Failed to parse acccess token: {:?}", e);
        exit(1);
    });

    let access_token = json
        .get("access_token")
        .unwrap_or_else(|| {
            eprintln!("Failed to parse acccess token: {:?}", json);
            exit(1);
        })
        .as_str()
        .unwrap_or_else(|| {
            eprintln!("Failed to parse acccess token: {:?}", json);
            exit(1);
        });

    access_token.to_string()
}

fn fetch_streams(access_token: &str, after: Option<String>) -> (Vec<Entry>, Option<String>) {
    let agent = configure_agent();

    let client_id = env::var("TWITCH_CLIENT_ID").unwrap_or_else(|_| {
        eprintln!("Client id missing. Please set the TWITCH_CLIENT_ID environment variable.");
        exit(1);
    });

    // -----------------------------------------------------------------------------
    //     - Request -
    // -----------------------------------------------------------------------------
    let url = match after {
        Some(after) => format!("{}&after={}", ROOT_URL, after),
        None => ROOT_URL.to_string(),
    };

    let resp = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {}", access_token))
        .set("Client-Id", &client_id)
        .call()
        .unwrap_or_else(|e| {
            eprintln!("Failed to get streams: {:?}", e);
            exit(1);
        });

    let json: Value = resp.into_json().unwrap_or_else(|e| {
        eprintln!("Failed to deserialize json: {:?}", e);
        exit(1);
    });

    let pagination = json
        .get("pagination")
        .and_then(|v| v.get("cursor"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    let data = match json.get("data") {
        Some(Value::Array(a)) => a.iter().map(to_entry).collect::<Vec<_>>(),
        _ => exit(0),
    };

    (data, pagination)
}

// -----------------------------------------------------------------------------
//     - Excluded terms -
// -----------------------------------------------------------------------------
fn exclusions(exclude: Option<Vec<String>>) -> Vec<String> {
    let mut excluded = match exclude {
        Some(exclusions) => exclusions.iter().map(|x| x.to_lowercase()).collect(),
        None => vec![],
    };

    if let Ok(ignore_list) = env::var("TWITCH_IGNORE") {
        excluded.extend(ignore_list.split(',').map(str::to_lowercase));
    }

    excluded
}

// -----------------------------------------------------------------------------
//     - Main -
// -----------------------------------------------------------------------------
fn main() {
    let args = Args::parse();
    let search_term = args.term;
    let word_boundary = args.word;
    let lang = args.lang;

    let exclude = exclusions(args.exclude);

    println!("Searching for \"{}\"", search_term);

    let access_token = aquire_access_token();

    let mut table: Table<5> = Table::new();
    table.set_align(2, Align::Right);
    table.set_align(3, Align::Right);

    let mut total = 0;
    let mut page = None;
    loop {
        let (entries, next_page) = fetch_streams(&access_token, page);
        total += entries.len();
        page = next_page;

        for e in entries {
            if filter(&e, word_boundary, &search_term, &exclude, &lang) {
                table.push(format_row(e));
            }
        }

        if page.is_none() {
            break;
        }
    }

    table.print();

    println!("Done ({}/{})", table.len(), total);
}
