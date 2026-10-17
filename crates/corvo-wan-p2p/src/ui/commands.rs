//! Parses a typed line into a message or a command. Pure: it knows nothing
//! about tabs or people, so `[name]` arguments come back as typed.

use corvo_wan_p2p_core::identity::{Code, clean_name};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Code,
    Add(Code),
    Chat(String),
    Contacts,
    Requests,
    /// The request named, or the one in the open tab.
    Accept(Option<String>),
    /// The person named, or the one in the open tab.
    Block(Option<String>),
    Unblock(String),
    Blocked,
    Remove(String),
    Info(String),
    /// Already checked with `clean_name`.
    Name(String),
    Passphrase,
    Phrase,
    PhraseSave,
    Close,
    Clear,
    Update {
        yes: bool,
    },
    Welcome,
    Help,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Nothing but spaces.
    Empty,
    /// A message for the open chat, as typed.
    Text(String),
    Cmd(Cmd),
    /// What was wrong with the command, ready to show.
    Wrong(String),
    /// A known command typed without what it needs, or with the wrong words:
    /// its name, for its [`Usage`] card.
    Usage(&'static str),
}

/// Every command name `parse` accepts, aliases included, in the order "did you
/// mean" prefers them.
pub const COMMANDS: &[&str] = &[
    "code",
    "add",
    "chat",
    "contacts",
    "requests",
    "accept",
    "block",
    "unblock",
    "blocked",
    "remove",
    "info",
    "name",
    "passphrase",
    "phrase",
    "close",
    "clear",
    "update",
    "welcome",
    "help",
    "h",
    "quit",
    "q",
];

/// What `/help` lists, by group: each command and what it does.
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "You",
        &[
            ("/name <new name>", "your display name"),
            ("/passphrase", "change your passphrase"),
            ("/phrase", "your 12 words"),
            ("/phrase save", "save them to a file"),
        ],
    ),
    (
        "Friends",
        &[
            ("/code", "your code"),
            ("/add <code>", "add a friend"),
            ("/contacts", "contacts and requests"),
            ("/requests", "people waiting for you"),
            ("/blocked", "blocked people"),
            ("/accept [name]", "accept a request"),
            ("/block [name]", "block someone"),
            ("/unblock <name>", "unblock someone"),
            ("/remove <name>", "delete a contact"),
            ("/info <name>", "about a contact"),
        ],
    ),
    (
        "Chatting",
        &[("/chat <name>", "open a chat"), ("/close", "close this tab"), ("/clear", "clear the screen (Ctrl-L)")],
    ),
    (
        "App",
        &[
            ("/update", "get the newest version"),
            ("/welcome", "the welcome again"),
            ("/help  /h", "this list"),
            ("/quit  /q", "quit (Ctrl-C)"),
        ],
    ),
];

/// The keys, for `/help`.
pub const KEYS: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Shift-Tab, Alt-1..9", "switch tabs"),
    ("Alt+Enter", "new line"),
    ("PgUp/PgDn", "scroll"),
    ("↑/↓", "what you typed before"),
];

/// How a command is typed, what it does in one sentence, and an example:
/// the card shown when it's typed wrong.
#[derive(Debug, PartialEq, Eq)]
pub struct Usage {
    pub usage: &'static str,
    pub what: &'static str,
    pub example: Option<&'static str>,
}

const fn card(usage: &'static str, what: &'static str, example: Option<&'static str>) -> Usage {
    Usage { usage, what, example }
}

/// Each command's card, by its name (not an alias).
const USAGES: &[(&str, Usage)] = &[
    ("code", card("/code", "Shows your code and offers to save it to a file.", None)),
    ("add", card("/add <code>", "Adds a friend by their code and opens a chat with them.", Some("/add corvo1…"))),
    ("chat", card("/chat <name>", "Opens a chat with a contact or a request.", Some("/chat rahul"))),
    ("contacts", card("/contacts", "Lists your contacts A to Z, then your requests.", None)),
    ("requests", card("/requests", "Lists the people who messaged you but aren't contacts yet.", None)),
    (
        "accept",
        card("/accept [name]", "Accepts a request: the one named, or the one in this tab.", Some("/accept rahul")),
    ),
    ("block", card("/block [name]", "Blocks someone: the one named, or the one in this tab.", Some("/block rahul"))),
    ("unblock", card("/unblock <name>", "Lets someone you blocked reach you again.", Some("/unblock rahul"))),
    ("blocked", card("/blocked", "Lists the people you've blocked.", None)),
    (
        "remove",
        card(
            "/remove <name>",
            "Deletes a contact, and asks whether to delete your history too.",
            Some("/remove rahul"),
        ),
    ),
    ("info", card("/info <name>", "Shows a contact's code, name and when they were last seen.", Some("/info rahul"))),
    ("name", card("/name <new name>", "Changes the name your friends see.", Some("/name Ana Maria"))),
    ("passphrase", card("/passphrase", "Changes your passphrase. Type it alone: it asks for them hidden.", None)),
    (
        "phrase",
        card(
            "/phrase [save]",
            "Shows your 12 words, or saves them to a file. It asks for your passphrase hidden.",
            Some("/phrase save"),
        ),
    ),
    ("close", card("/close", "Closes this chat tab.", None)),
    ("clear", card("/clear", "Clears the screen. Ctrl-L does too.", None)),
    (
        "update",
        card(
            "/update [yes]",
            "Looks for a newer version; with yes it installs it without asking.",
            Some("/update yes"),
        ),
    ),
    ("welcome", card("/welcome", "Shows the welcome again.", None)),
    ("help", card("/help  /h", "Lists the commands and keys.", None)),
    ("quit", card("/quit  /q", "Quits the app. Ctrl-C does too.", None)),
];

/// The card for the command called `name`, alias or not.
pub fn usage(name: &str) -> Option<&'static Usage> {
    card_of(name).map(|(_, usage)| usage)
}

/// The card for `name`, with the name it's kept under.
fn card_of(name: &str) -> Option<&'static (&'static str, Usage)> {
    let name = match name {
        "h" => "help",
        "q" => "quit",
        other => other,
    };
    USAGES.iter().find(|(n, _)| *n == name)
}

/// Names that were never commands here but people type anyway, and what they meant.
const MEANT: &[(&str, &str)] = &[("exit", "quit")];

pub fn parse(line: &str) -> Input {
    if line.trim().is_empty() {
        return Input::Empty;
    }
    if let Some(escaped) = line.strip_prefix("//") {
        return Input::Text(format!("/{escaped}"));
    }
    let Some(rest) = line.trim().strip_prefix('/') else {
        return Input::Text(line.to_string());
    };
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    let name = name.to_lowercase();
    let words: Vec<&str> = args.split_whitespace().collect();
    let cmd = match name.as_str() {
        "" => return Input::Wrong("type a command after the /, or /help".into()),
        "code" => no_args(args, Cmd::Code),
        "contacts" => no_args(args, Cmd::Contacts),
        "requests" => no_args(args, Cmd::Requests),
        "blocked" => no_args(args, Cmd::Blocked),
        "passphrase" => no_args(args, Cmd::Passphrase),
        "close" => no_args(args, Cmd::Close),
        "clear" => no_args(args, Cmd::Clear),
        "welcome" => no_args(args, Cmd::Welcome),
        "help" | "h" => no_args(args, Cmd::Help),
        "quit" | "q" => no_args(args, Cmd::Quit),
        "add" if args.is_empty() => Err(None),
        "add" => args.parse().map(Cmd::Add).map_err(|e| Some(e.to_string())),
        "chat" => named(args).map(Cmd::Chat),
        "unblock" => named(args).map(Cmd::Unblock),
        "remove" => named(args).map(Cmd::Remove),
        "info" => named(args).map(Cmd::Info),
        "accept" => Ok(Cmd::Accept(optional(args))),
        "block" => Ok(Cmd::Block(optional(args))),
        "name" if args.is_empty() => Err(None),
        "name" => clean_name(args).map(Cmd::Name).map_err(|e| Some(e.to_string())),
        "phrase" => match words.as_slice() {
            [] => Ok(Cmd::Phrase),
            ["save"] => Ok(Cmd::PhraseSave),
            _ => Err(None),
        },
        "update" => match words.as_slice() {
            [] => Ok(Cmd::Update { yes: false }),
            ["yes" | "y"] => Ok(Cmd::Update { yes: true }),
            _ => Err(None),
        },
        other => Err(Some(unknown(other))),
    };
    match cmd {
        Ok(cmd) => Input::Cmd(cmd),
        Err(Some(why)) => Input::Wrong(why),
        // Every name that gets here has a card.
        Err(None) => Input::Usage(card_of(&name).map_or("help", |(name, _)| name)),
    }
}

/// Whether `line` may hold a passphrase: `/passphrase` or `/phrase` with
/// more after it than they take. Such a line isn't kept in the input history.
pub fn may_hold_a_secret(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix('/') else { return false };
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let words: Vec<&str> = args.split_whitespace().collect();
    match name.to_lowercase().as_str() {
        "passphrase" => !words.is_empty(),
        "phrase" => !matches!(words.as_slice(), [] | ["save"]),
        _ => false,
    }
}

/// A command, or why not: `None` for its usage card, else what was wrong.
type Parsed<T> = Result<T, Option<String>>;

fn no_args(args: &str, cmd: Cmd) -> Parsed<Cmd> {
    if args.is_empty() { Ok(cmd) } else { Err(None) }
}

fn named(args: &str) -> Parsed<String> {
    if args.is_empty() { Err(None) } else { Ok(args.to_string()) }
}

fn optional(args: &str) -> Option<String> {
    (!args.is_empty()).then(|| args.to_string())
}

/// "unknown command", with a "did you mean" when a known name is close.
fn unknown(name: &str) -> String {
    match suggest(name) {
        Some(known) => format!("unknown command /{name}: did you mean /{known}?"),
        None => format!("unknown command /{name}, try /help"),
    }
}

/// What someone probably meant: the closest command within 2 edits, and closer
/// than the name is long so one letter doesn't guess. Ties go to the earlier
/// entry in `COMMANDS`.
fn suggest(name: &str) -> Option<&'static str> {
    if let Some((_, meant)) = MEANT.iter().find(|(typed, _)| *typed == name) {
        return Some(meant);
    }
    let len = name.chars().count();
    COMMANDS
        .iter()
        .map(|c| (distance(name, c), *c))
        .filter(|(d, _)| *d <= 2 && *d < len)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Levenshtein distance in chars.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

impl Cmd {
    /// How it's typed.
    #[cfg(test)]
    pub fn typed(&self) -> &'static str {
        match self {
            Cmd::Code => "/code",
            Cmd::Add(_) => "/add",
            Cmd::Chat(_) => "/chat",
            Cmd::Contacts => "/contacts",
            Cmd::Requests => "/requests",
            Cmd::Accept(_) => "/accept",
            Cmd::Block(_) => "/block",
            Cmd::Unblock(_) => "/unblock",
            Cmd::Blocked => "/blocked",
            Cmd::Remove(_) => "/remove",
            Cmd::Info(_) => "/info",
            Cmd::Name(_) => "/name",
            Cmd::Passphrase => "/passphrase",
            Cmd::Phrase => "/phrase",
            Cmd::PhraseSave => "/phrase save",
            Cmd::Close => "/close",
            Cmd::Clear => "/clear",
            Cmd::Update { .. } => "/update",
            Cmd::Welcome => "/welcome",
            Cmd::Help => "/help",
            Cmd::Quit => "/quit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corvo_wan_p2p_core::identity::Seed;

    fn cmd(line: &str) -> Cmd {
        match parse(line) {
            Input::Cmd(cmd) => cmd,
            other => panic!("{line:?} parsed as {other:?}"),
        }
    }

    fn wrong(line: &str) -> String {
        match parse(line) {
            Input::Wrong(why) => why,
            other => panic!("{line:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn every_command_is_in_the_help_once() {
        let usages: Vec<&str> =
            HELP.iter().flat_map(|(_, commands)| commands.iter().map(|(usage, _)| *usage)).collect();
        for name in COMMANDS {
            let slash = format!("/{name}");
            let found = usages.iter().filter(|u| u.split_whitespace().any(|word| word == slash)).count();
            // /phrase save is listed beside /phrase.
            let want = if *name == "phrase" { 2 } else { 1 };
            assert_eq!(found, want, "{slash} in the help: {usages:?}");
        }
        assert_eq!(HELP.iter().map(|(group, _)| *group).collect::<Vec<_>>(), ["You", "Friends", "Chatting", "App"]);
    }

    #[test]
    fn every_command_has_a_usage() {
        for name in COMMANDS {
            let card = usage(name).unwrap_or_else(|| panic!("/{name} has no usage"));
            let slash = format!("/{name}");
            assert!(card.usage.split_whitespace().any(|w| w == slash), "/{name}: {card:?}");
            assert!(card.what.ends_with('.'), "/{name}: one sentence");
            assert!(card.example.is_none_or(|e| e.starts_with(&slash)), "/{name}: {card:?}");
        }
        assert_eq!(usage("h"), usage("help"), "aliases share it");
        assert_eq!(usage("q"), usage("quit"));
        assert_eq!(usage("nope"), None);
    }

    #[test]
    fn a_command_missing_its_name_gives_its_usage() {
        assert_eq!(parse("/chat"), Input::Usage("chat"));
        assert_eq!(parse("/add"), Input::Usage("add"));
        assert_eq!(parse("/q now"), Input::Usage("quit"), "under its name, not the alias");
    }

    #[test]
    fn a_bad_code_is_still_a_one_line_error() {
        let text = Seed::generate().code().to_string();
        assert!(wrong(&format!("/add {}", &text[..30])).contains("probably missing"));
        assert!(wrong("/name a\u{202E}b").contains("can't"), "a bad name too");
        assert!(wrong("/frobnicate").starts_with("unknown command"));
    }

    #[test]
    fn plain_text_empty_lines_and_the_slash_escape() {
        assert_eq!(parse("hello there "), Input::Text("hello there ".into()));
        assert_eq!(parse("  \n "), Input::Empty);
        assert_eq!(parse("//shrug"), Input::Text("/shrug".into()));
        assert_eq!(parse("a /b"), Input::Text("a /b".into()));
    }

    #[test]
    fn commands_without_arguments() {
        for (line, want) in [
            ("/code", Cmd::Code),
            ("/contacts", Cmd::Contacts),
            ("/requests", Cmd::Requests),
            ("/blocked", Cmd::Blocked),
            ("/passphrase", Cmd::Passphrase),
            ("/phrase", Cmd::Phrase),
            ("/close", Cmd::Close),
            ("/clear", Cmd::Clear),
            ("/welcome", Cmd::Welcome),
            ("/help", Cmd::Help),
            ("/h", Cmd::Help),
            ("/quit", Cmd::Quit),
            ("/q", Cmd::Quit),
            ("  /quit  ", Cmd::Quit),
            ("/QUIT", Cmd::Quit),
        ] {
            assert_eq!(cmd(line), want, "{line}");
        }
    }

    #[test]
    fn commands_without_arguments_refuse_extra_ones() {
        for (line, name) in [
            ("/code now", "code"),
            ("/contacts all", "contacts"),
            ("/requests x", "requests"),
            ("/blocked x", "blocked"),
            ("/passphrase new", "passphrase"),
            ("/close bob", "close"),
            ("/clear all", "clear"),
            ("/help me", "help"),
            ("/q now", "quit"),
        ] {
            assert_eq!(parse(line), Input::Usage(name), "{line}");
        }
    }

    #[test]
    fn add_takes_a_code_and_checks_it() {
        let code = Seed::generate().code();
        assert_eq!(cmd(&format!("/add {code}")), Cmd::Add(code));
        let text = code.to_string();
        let (a, b) = text.split_at(20);
        assert_eq!(cmd(&format!("/add {a} {b}")), Cmd::Add(code), "pasted with a break in it");
        assert_eq!(parse("/add"), Input::Usage("add"));
        assert!(wrong(&format!("/add {}", &text[..30])).contains("probably missing"));
        // One character of the key changed: only the checksum can tell.
        let swapped = if &text[10..11] == "q" { "p" } else { "q" };
        let typo = format!("{}{swapped}{}", &text[..10], &text[11..]);
        assert!(wrong(&format!("/add {typo}")).contains("typo"));
    }

    #[test]
    fn commands_that_need_a_name() {
        assert_eq!(cmd("/chat rahul"), Cmd::Chat("rahul".into()));
        assert_eq!(cmd("/chat  ana maria "), Cmd::Chat("ana maria".into()));
        assert_eq!(cmd("/unblock rahul·m4x8"), Cmd::Unblock("rahul·m4x8".into()));
        assert_eq!(cmd("/remove rahul"), Cmd::Remove("rahul".into()));
        assert_eq!(cmd("/info rahul"), Cmd::Info("rahul".into()));
        for (line, name) in [("/chat", "chat"), ("/unblock ", "unblock"), ("/remove", "remove"), ("/info", "info")] {
            assert_eq!(parse(line), Input::Usage(name), "{line}");
        }
    }

    #[test]
    fn accept_and_block_take_a_name_or_mean_this_tab() {
        assert_eq!(cmd("/accept"), Cmd::Accept(None));
        assert_eq!(cmd("/accept rahul"), Cmd::Accept(Some("rahul".into())));
        assert_eq!(cmd("/block"), Cmd::Block(None));
        assert_eq!(cmd("/block ana maria"), Cmd::Block(Some("ana maria".into())));
    }

    #[test]
    fn name_is_checked_like_any_display_name() {
        assert_eq!(cmd("/name  Ana   Maria "), Cmd::Name("Ana Maria".into()));
        assert_eq!(parse("/name"), Input::Usage("name"));
        assert!(wrong(&format!("/name {}", "x".repeat(33))).contains("33 characters"));
        assert!(wrong("/name a\u{202E}b").contains("can't"));
    }

    #[test]
    fn phrase_and_update_take_a_fixed_word() {
        assert_eq!(cmd("/phrase save"), Cmd::PhraseSave);
        assert_eq!(parse("/phrase copy"), Input::Usage("phrase"), "no clipboard any more");
        assert_eq!(parse("/phrase print"), Input::Usage("phrase"));
        assert_eq!(parse("/phrase save now"), Input::Usage("phrase"));
        assert_eq!(cmd("/update"), Cmd::Update { yes: false });
        assert_eq!(cmd("/update yes"), Cmd::Update { yes: true });
        assert_eq!(cmd("/update y"), Cmd::Update { yes: true });
        assert_eq!(parse("/update now"), Input::Usage("update"));
    }

    #[test]
    fn typos_get_a_did_you_mean() {
        assert_eq!(wrong("/acept"), "unknown command /acept: did you mean /accept?");
        assert_eq!(wrong("/acept rahul"), "unknown command /acept: did you mean /accept?");
        assert_eq!(wrong("/hepl"), "unknown command /hepl: did you mean /help?");
        assert_eq!(wrong("/contact"), "unknown command /contact: did you mean /contacts?");
        assert_eq!(wrong("/exit"), "unknown command /exit: did you mean /quit?");
        assert_eq!(wrong("/x"), "unknown command /x, try /help");
        assert_eq!(wrong("/frobnicate"), "unknown command /frobnicate, try /help");
        assert_eq!(wrong("/"), "type a command after the /, or /help");
    }

    #[test]
    fn every_listed_command_parses_and_says_how_it_is_typed() {
        let code = Seed::generate().code();
        for name in COMMANDS {
            let line = match *name {
                "add" => format!("/add {code}"),
                "chat" | "unblock" | "remove" | "info" | "name" => format!("/{name} rahul"),
                _ => format!("/{name}"),
            };
            let parsed = cmd(&line);
            let typed = parsed.typed();
            assert!(line.starts_with(typed) || ["/h", "/q"].contains(&line.as_str()), "{line} says {typed}");
        }
    }
}
