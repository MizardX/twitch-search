use std::io::{self, Write};
use std::{cmp, env};

use chrono::Duration;
use chrono::prelude::*;
use clap::Parser;
use serde_json::Value;
use thiserror::Error;
use ureq::Response;

// -----------------------------------------------------------------------------
//     - Errors -
// -----------------------------------------------------------------------------

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

// -----------------------------------------------------------------------------
//     - Command line arguments -
// -----------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// Terms to search for
    #[clap(default_value = "")]
    term: Vec<String>,

    /// Streamers to exclude, combined with the TWITCH_IGNORE environment variable
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
//     - Filtering -
// -----------------------------------------------------------------------------

struct Filter {
    /// Terms to search for
    search_terms: Vec<String>,
    /// Search on word boundary
    word_boundary: bool,
    /// Require matching all words, instead of just any
    all: bool,
    /// Only show language (en, fr, ...)
    lang: Option<String>,
    /// Streamers to exclude
    exclude: Vec<String>,
}

impl Filter {
    fn from_args(args: Args) -> Self {
        let mut exclude = args.exclude.unwrap_or_default();

        if let Ok(ignore_list) = env::var("TWITCH_IGNORE") {
            exclude.extend(ignore_list.split(',').map(str::to_string));
        }

        Filter {
            exclude,
            search_terms: args.term,
            word_boundary: args.word,
            all: args.all,
            lang: args.lang,
        }
    }
}

// -----------------------------------------------------------------------------
//     - String comparison -
// -----------------------------------------------------------------------------

trait IgnoreCase {
    fn eq_ignore_case(&self, other: &str) -> bool;
    fn contains_ignore_case(&self, needle: &str) -> bool;
}

impl IgnoreCase for str {
    // Compares char-by-char via `to_lowercase()` iterators, so no owned lowercased copy is needed.
    fn eq_ignore_case(&self, other: &str) -> bool {
        self.chars()
            .flat_map(char::to_lowercase)
            .eq(other.chars().flat_map(char::to_lowercase))
    }

    fn contains_ignore_case(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }

        let mut rest = self;
        loop {
            let mut h = rest.chars().flat_map(char::to_lowercase);
            let mut n = needle.chars().flat_map(char::to_lowercase);
            let matched = loop {
                match n.next() {
                    None => break true,
                    Some(nc) => match h.next() {
                        Some(hc) if hc == nc => continue,
                        _ => break false,
                    },
                }
            };
            if matched {
                return true;
            }

            let mut chars = rest.chars();
            if chars.next().is_none() {
                return false;
            }
            rest = chars.as_str();
        }
    }
}

// -----------------------------------------------------------------------------
//     - Entry -
// -----------------------------------------------------------------------------

#[derive(Debug)]
struct Entry {
    lang: String,
    display_name: String,
    title: String,
    viewer_count: i64,
    live_duration: Option<Duration>,
}

impl Entry {
    fn matches(&self, filter: &Filter) -> bool {
        if filter
            .exclude
            .iter()
            .any(|term| term.eq_ignore_ascii_case(&self.display_name))
        {
            return false;
        }

        if let Some(lang) = &filter.lang
            && &self.lang != lang
        {
            return false;
        }

        // fn items don't capture their environment, so both branches coerce to the same fn pointer type.
        fn whole_word(title: &str, term: &str) -> bool {
            title
                .split(|c: char| !c.is_alphabetic())
                .any(|token| token.eq_ignore_case(term))
        }

        fn substring(title: &str, term: &str) -> bool {
            title.contains_ignore_case(term)
        }

        let is_match: fn(&str, &str) -> bool = if filter.word_boundary {
            whole_word
        } else {
            substring
        };

        if filter.all {
            filter
                .search_terms
                .iter()
                .all(|term| is_match(&self.title, term))
        } else {
            filter
                .search_terms
                .iter()
                .any(|term| is_match(&self.title, term))
        }
    }
}

macro_rules! take_str {
    ($val: expr, $key: expr, $err: expr) => {
        match $val.get_mut($key).map(Value::take) {
            Some(Value::String(s)) => s,
            _ => return Err($err),
        }
    };
}

macro_rules! to_num {
    ($val: expr, $key: expr) => {
        $val.get($key).unwrap().as_i64().unwrap()
    };
}

fn to_instant(ds: &str) -> Option<Duration> {
    let val = ds.parse::<DateTime<Utc>>().ok()?;
    Some(Utc::now() - val)
}

impl TryFrom<&mut Value> for Entry {
    type Error = AppError;

    fn try_from(value: &mut Value) -> Result<Self, Self::Error> {
        Ok(Entry {
            lang: take_str!(value, "language", AppError::ParseJson),
            display_name: take_str!(value, "user_name", AppError::ParseJson),
            title: take_str!(value, "title", AppError::ParseJson),
            viewer_count: to_num!(value, "viewer_count"),
            live_duration: to_instant(&take_str!(value, "started_at", AppError::ParseJson)),
        })
    }
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

// Columns: 0 = lang, 1 = url, 2 = viewers, 3 = duration, 4 (unpadded) = title.
#[derive(Debug)]
struct Table {
    align: [Align; 4],
    widths: [usize; 4],
    rows: Vec<Entry>,
}

impl Table {
    fn new() -> Self {
        Table {
            align: [Align::Left; 4],
            widths: [0; 4],
            rows: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn set_align(&mut self, column: usize, align: Align) {
        self.align[column] = align;
    }

    fn push(&mut self, entry: Entry) {
        let cell_widths = [
            entry.lang.len(),
            "https://twitch.tv/".len() + entry.display_name.len(),
            Self::digit_count(entry.viewer_count) + " viewers".len(),
            Self::duration_width(&entry.live_duration),
        ];
        for (width, cell) in self.widths.iter_mut().zip(cell_widths) {
            *width = cmp::max(*width, cell);
        }
        self.rows.push(entry);
    }

    fn digit_count(n: i64) -> usize {
        if n == 0 {
            return 1;
        }
        let mut n = n.unsigned_abs();
        let mut count = 0;
        while n > 0 {
            count += 1;
            n /= 10;
        }
        count
    }

    // Width of "HH:MM", where HH is at least 2 digits but grows for longer streams.
    fn duration_width(duration: &Option<Duration>) -> usize {
        match duration {
            Some(d) => Self::digit_count(d.num_hours()).max(2) + 1 + 2,
            None => 0,
        }
    }

    fn write_aligned(
        out: &mut impl Write,
        align: Align,
        width: usize,
        value: impl std::fmt::Display,
    ) -> io::Result<()> {
        match align {
            Align::Left => write!(out, "{value:<width$} | "),
            Align::Center => write!(out, "{value:^width$} | "),
            Align::Right => write!(out, "{value:>width$} | "),
        }
    }

    fn print(&self) -> io::Result<()> {
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());

        for entry in &self.rows {
            Self::write_aligned(&mut out, self.align[0], self.widths[0], &entry.lang)?;
            Self::write_aligned(
                &mut out,
                self.align[1],
                self.widths[1],
                format_args!("https://twitch.tv/{}", entry.display_name),
            )?;
            Self::write_aligned(
                &mut out,
                self.align[2],
                self.widths[2],
                format_args!("{} viewers", entry.viewer_count),
            )?;
            match &entry.live_duration {
                Some(d) => Self::write_aligned(
                    &mut out,
                    self.align[3],
                    self.widths[3],
                    format_args!("{:02}:{:02}", d.num_hours(), d.num_minutes() % 60),
                )?,
                None => Self::write_aligned(&mut out, self.align[3], self.widths[3], "")?,
            }
            for c in entry.title.chars() {
                write!(out, "{}", if c.is_control() { ' ' } else { c })?;
            }
            writeln!(out)?;
        }

        out.flush()
    }
}

// -----------------------------------------------------------------------------
//     - Twitch API -
// -----------------------------------------------------------------------------

const ROOT_URL: &str =
    "https://api.twitch.tv/helix/streams?first=100&game_id=1469308723&game_id=509658";

// Reuses one String buffer across pages instead of allocating a new URL each time.
struct StreamsUrl {
    buf: String,
    base_len: usize,
}

impl StreamsUrl {
    fn new() -> Self {
        let buf = String::from(ROOT_URL);
        let base_len = buf.len();
        StreamsUrl { buf, base_len }
    }

    fn set_after(&mut self, after: Option<&str>) -> &str {
        self.buf.truncate(self.base_len);
        if let Some(after) = after {
            self.buf.push_str("&after=");
            self.buf.push_str(after);
        }
        &self.buf
    }
}

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

struct Limits {
    limit: i64,
    remaining: i64,
    reset: DateTime<Utc>,
}

impl Limits {
    fn from_response(resp: &Response) -> Option<Self> {
        Some(Self {
            limit: resp.header("Ratelimit-Limit")?.parse::<i64>().ok()?,
            remaining: resp.header("Ratelimit-Remaining")?.parse::<i64>().ok()?,
            reset: DateTime::from_timestamp(
                resp.header("Ratelimit-Reset")?.parse::<i64>().ok()?,
                0,
            )?,
        })
    }

    fn print_warning_if_low(&self) {
        if self.remaining < 100 {
            println!(
                "{} of {} used, reset in {}",
                self.remaining,
                self.limit,
                self.reset.signed_duration_since(Local::now())
            );
        }
    }
}

struct FetchStreams {
    entries: Vec<Entry>,
    next_page: Option<String>,
    limits: Option<Limits>,
}

/// Bundles the Twitch API connection with the search filter and result table.
struct TwitchClient {
    agent: ureq::Agent,
    client_id: String,
    auth_header: String,
    url: StreamsUrl,
    filter: Filter,
    table: Table,
}

impl TwitchClient {
    fn new(filter: Filter) -> Result<Self, AccessTokenError> {
        let agent = configure_agent();
        let client_id =
            env::var("TWITCH_CLIENT_ID").map_err(|_| AccessTokenError::MissingClientId)?;
        let access_token = Self::aquire_access_token(&agent, &client_id)?;

        let mut table = Table::new();
        table.set_align(2, Align::Right);
        table.set_align(3, Align::Right);

        Ok(Self {
            agent,
            client_id,
            auth_header: format!("Bearer {}", access_token),
            url: StreamsUrl::new(),
            filter,
            table,
        })
    }

    fn aquire_access_token(
        agent: &ureq::Agent,
        client_id: &str,
    ) -> Result<String, AccessTokenError> {
        let client_secret =
            env::var("TWITCH_CLIENT_SECRET").map_err(|_| AccessTokenError::MissingClientSecret)?;

        let resp = agent
            .post("https://id.twitch.tv/oauth2/token")
            .send_form(&[
                ("client_id", client_id),
                ("client_secret", &client_secret),
                ("grant_type", "client_credentials"),
            ])?;

        let mut json = resp.into_json::<Value>()?;

        Ok(take_str!(
            json,
            "access_token",
            AccessTokenError::ParseAccessToken
        ))
    }

    fn fetch_page(&mut self, after: Option<&str>) -> Result<FetchStreams, AppError> {
        let url = self.url.set_after(after);

        let resp = self
            .agent
            .get(url)
            .set("Authorization", &self.auth_header)
            .set("Client-Id", &self.client_id)
            .call()?;

        let limits = Limits::from_response(&resp);

        let mut json: Value = resp.into_json()?;

        let next_page = match json
            .get_mut("pagination")
            .and_then(|v| v.get_mut("cursor"))
            .map(Value::take)
        {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };

        let entries = match json.get_mut("data") {
            Some(Value::Array(a)) => a
                .iter_mut()
                .map(Entry::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            _ => Err(AppError::ParseJson)?,
        };

        Ok(FetchStreams {
            entries,
            next_page,
            limits,
        })
    }

    /// Fetches every page, filling `self.table` with matching entries. Prints a progress dot per page.
    fn fetch_all(&mut self) -> Result<(usize, Option<Limits>), AppError> {
        let mut total = 0;
        let mut page = None;
        let mut last_limits;

        let stdout = io::stdout();
        let mut out = stdout.lock();

        loop {
            let streams = self.fetch_page(page.as_deref())?;

            write!(out, ".")?;
            out.flush()?;

            total += streams.entries.len();
            page = streams.next_page;

            for entry in streams.entries {
                if entry.matches(&self.filter) {
                    self.table.push(entry);
                }
            }

            last_limits = streams.limits;

            if page.is_none() {
                break;
            }
        }

        writeln!(out)?;

        Ok((total, last_limits))
    }
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
    let filter = Filter::from_args(args);

    println!("Searching for {:?}", filter.search_terms);

    let mut client = TwitchClient::new(filter)?;

    let (total, last_limits) = client.fetch_all()?;

    client.table.print()?;

    let matched = client.table.len();
    println!("Done ({matched}/{total})");

    if let Some(limits) = last_limits {
        limits.print_warning_if_low();
    }

    Ok(())
}
