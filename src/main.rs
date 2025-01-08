use std::io::Write;
use std::{cmp, env};

use chrono::prelude::*;
use clap::Parser;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
enum AccessTokenError {
    #[error("Client id missing. Please set the TWITCH_CLIENT_ID environment variable.")]
    MissingClientId,

    #[error("Client secret missing. Please set the TWITCH_CLIENT_SECRET environment variable.")]
    MissingClientSecret,

    #[error("Failed to get acccess token: {0}")]
    RequestError(Box<ureq::Error>),

    #[error("Failed to read acccess token: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse acccess token: {0}")]
    ParseAccessTokenJson(#[from] serde_json::Error),

    #[error("Failed to parse acccess token.")]
    ParseAccessToken,
}

impl From<ureq::Error> for AccessTokenError {
    fn from(e: ureq::Error) -> Self {
        AccessTokenError::RequestError(Box::new(e))
    }
}

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    AccessToken(#[from] AccessTokenError),

    #[error("Failed to get streams: {0}")]
    FetchStreams(Box<ureq::Error>),

    #[error("Failed to read streams: {0}")]
    ReadStreams(#[from] std::io::Error),

    #[error("Failed to deserialize json: {0}")]
    DeserializeJson(#[from] serde_json::Error),

    #[error("Failed to parse json.")]
    ParseJson,
}

impl From<ureq::Error> for AppError {
    fn from(e: ureq::Error) -> Self {
        AppError::FetchStreams(Box::new(e))
    }
}

const ROOT_URL: &str =
    "https://api.twitch.tv/helix/streams?first=100&game_id=1469308723";

// -----------------------------------------------------------------------------
//     - Command line arguments -
// -----------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// Term to search for
    #[clap(default_value = "")]
    term: Vec<String>,

    /// Streamers to exclude
    #[clap(short = 'x', long)]
    exclude: Option<Vec<String>>,

    /// Only show langauge (en, fr, ...)
    #[clap(short = 'l', long)]
    lang: Option<String>,

    /// Require matching all words, instead of just any
    #[clap(short, long)]
    all: bool,

    /// Search on word boundary
    #[clap(short, long)]
    word: bool,
}

// -----------------------------------------------------------------------------
//     - Table formatting -
// -----------------------------------------------------------------------------

#[derive(Debug)]
struct Entry {
    lang: String,
    display_name: String,
    title: String,
    viewer_count: i64,
    live_duration: String,
}

impl Entry {
    fn matches(
        &self,
        whole_word: bool,
        all: bool,
        term: &[String],
        ignored_names: &[String],
        lang: &Option<String>,
    ) -> bool {
        if ignored_names.contains(&self.display_name.to_lowercase()) {
            return false;
        }

        if let Some(lang) = lang {
            if &self.lang != lang {
                return false;
            }
        }

        if whole_word {
            for e in self
                .title
                .to_lowercase()
                .split(|c: char| !c.is_alphabetic())
            {
                if term.iter().any(|t| t.eq(e)) {
                    return true;
                }
            }
            return false;
        }

        let lower_title = self.title.to_lowercase();
        if all {
            term.iter().all(|t| lower_title.contains(t))
        } else {
            term.iter().any(|t| lower_title.contains(t))
        }
    }

    fn format_row(self) -> [String; 5] {
        [
            self.lang,
            format!("https://twitch.tv/{}", self.display_name),
            format!("{} viewers", self.viewer_count),
            self.live_duration,
            self.title.replace(|c: char| c.is_control(), " "),
        ]
    }
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

impl From<&Value> for Entry {
    fn from(value: &Value) -> Self {
        Entry {
            lang: to_str!(value, "language"),
            display_name: to_str!(value, "user_name"),
            title: to_str!(value, "title"),
            viewer_count: to_num!(value, "viewer_count"),
            live_duration: to_instant(&to_str!(value, "started_at")),
        }
    }
}

#[allow(unused)]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug)]
struct Table<const N: usize> {
    align: [Align; N],
    widths: [usize; N],
    rows: Vec<[String; N]>,
}

impl<const N: usize> Table<N> {
    fn new() -> Self {
        Table {
            align: [Align::Left; N],
            widths: [0; N],
            rows: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn set_align(&mut self, column: usize, align: Align) {
        self.align[column] = align;
    }

    fn push(&mut self, row: [String; N]) {
        for (width, cell) in self.widths.iter_mut().zip(&row).take(N - 1) {
            *width = cmp::max(*width, cell.len());
        }
        self.rows.push(row);
    }

    fn print(&self) {
        for row in &self.rows {
            for ((align, row), width) in self.align.iter().zip(row).zip(self.widths).take(N - 1) {
                match align {
                    Align::Left => print!("{row:<width$} | "),
                    Align::Center => print!("{row:^width$} | "),
                    Align::Right => print!("{row:>width$} | "),
                }
            }
            println!("{}", row[N - 1]); // last column always left aligned
        }
    }
}

// -----------------------------------------------------------------------------
//     - Request and parsing -
// -----------------------------------------------------------------------------

fn configure_agent() -> ureq::Agent {
    let proxy = env::var("https_proxy")
        .ok()
        .and_then(|p| ureq::Proxy::new(p).ok());

    let mut agent = ureq::AgentBuilder::new();
    if let Some(proxy) = proxy {
        agent = agent.proxy(proxy);
    }

    agent.build()
}

fn aquire_access_token() -> Result<String, AccessTokenError> {
    let agent = configure_agent();

    let client_id = env::var("TWITCH_CLIENT_ID").map_err(|_| AccessTokenError::MissingClientId)?;

    let client_secret =
        env::var("TWITCH_CLIENT_SECRET").map_err(|_| AccessTokenError::MissingClientSecret)?;

    let resp = agent
        .post("https://id.twitch.tv/oauth2/token")
        .send_form(&[
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("grant_type", "client_credentials"),
        ])?;

    let json = resp.into_json::<Value>()?;

    let access_token = json
        .get("access_token")
        .ok_or(AccessTokenError::ParseAccessToken)?
        .as_str()
        .ok_or(AccessTokenError::ParseAccessToken)?;

    Ok(access_token.to_string())
}

fn fetch_streams(
    access_token: &str,
    after: Option<String>,
) -> Result<(Vec<Entry>, Option<String>), AppError> {
    let agent = configure_agent();

    let client_id = env::var("TWITCH_CLIENT_ID").map_err(|_| AccessTokenError::MissingClientId)?;

    let url = match after {
        Some(after) => format!("{}&after={}", ROOT_URL, after),
        None => ROOT_URL.to_string(),
    };

    let resp = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {}", access_token))
        .set("Client-Id", &client_id)
        .call()?;

    let json: Value = resp.into_json()?;

    let pagination = json
        .get("pagination")
        .and_then(|v| v.get("cursor"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    let data = match json.get("data") {
        Some(Value::Array(a)) => a.iter().map(Into::into).collect::<Vec<_>>(),
        _ => Err(AppError::ParseJson)?,
    };

    Ok((data, pagination))
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
    run().unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });
}
fn run() -> Result<(), AppError> {
    let args = Args::parse();
    let search_terms = args.term;
    let word_boundary = args.word;
    let all = args.all;
    let lang = args.lang;

    let exclude = exclusions(args.exclude);

    println!("Searching for {search_terms:?}");

    let access_token = aquire_access_token()?;

    let mut table: Table<5> = Table::new();
    table.set_align(2, Align::Right);
    table.set_align(3, Align::Right);

    let mut total = 0;
    let mut page = None;
    loop {
        let (entries, next_page) = fetch_streams(&access_token, page)?;

        print!(".");
        std::io::stdout().flush()?;

        total += entries.len();
        page = next_page;

        for entry in entries {
            if entry.matches(word_boundary, all, &search_terms, &exclude, &lang) {
                table.push(entry.format_row());
            }
        }

        if page.is_none() {
            break;
        }
    }
    println!();

    table.print();

    let matched = table.len();
    println!("Done ({matched}/{total})");

    Ok(())
}
