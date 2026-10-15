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
    PhraseCopy,
    Close,
    Clear,
    Update {
        yes: bool,
    },
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
    "help",
    "h",
    "quit",
    "q",
];

/// What `/help` lists.
pub const HELP: &[(&str, &str)] = &[
    ("/code", "show your code and copy it (send it to friends any way you like)"),
    ("/add <code>", "add a friend by their code and open a chat"),
    ("/chat <name>", "open a chat (Tab completes names)"),
    ("/contacts", "your contacts, who's online, last seen"),
    ("/requests", "people who messaged you but aren't contacts yet"),
    ("/accept [name]", "accept a request (the one in this tab if no name)"),
    ("/block [name]", "block a request or contact (this tab if no name)"),
    ("/unblock <name>", "unblock someone"),
    ("/blocked", "list blocked people"),
    ("/remove <name>", "delete a contact (asks whether to delete the history too)"),
    ("/info <name>", "their code, name, online / last seen"),
    ("/name <new name>", "change your display name"),
    ("/passphrase", "change your history passphrase"),
    ("/phrase", "show your 12 recovery words again (asks your passphrase first)"),
    ("/phrase save", "write them to ~/corvo-wan-p2p-recovery.txt again"),
    ("/phrase copy", "copy them to the clipboard"),
    ("/close", "close this chat tab"),
    ("/clear", "clear the screen (Ctrl-L)"),
    ("/update [yes]", "check for and install a newer version"),
    ("/help  /h", "list commands"),
    ("/quit  /q", "quit (Ctrl-C)"),
];

/// The keys, for `/help`.
pub const KEYS: &str = "keys: Shift-Tab, Alt-←/→ or Alt-1..9 switch tabs · Alt+Enter new line · \
                        PgUp/PgDn or the wheel scroll · ↑/↓ earlier lines";

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
        "code" => no_args(args, "code", Cmd::Code),
        "contacts" => no_args(args, "contacts", Cmd::Contacts),
        "requests" => no_args(args, "requests", Cmd::Requests),
        "blocked" => no_args(args, "blocked", Cmd::Blocked),
        "passphrase" => no_args(args, "passphrase", Cmd::Passphrase),
        "close" => no_args(args, "close", Cmd::Close),
        "clear" => no_args(args, "clear", Cmd::Clear),
        "help" | "h" => no_args(args, "help", Cmd::Help),
        "quit" | "q" => no_args(args, "quit", Cmd::Quit),
        "add" if args.is_empty() => Err("try: /add <code>  (ask your friend for theirs; yours is /code)".into()),
        "add" => args.parse().map(Cmd::Add).map_err(|e| e.to_string()),
        "chat" => named(args, "chat").map(Cmd::Chat),
        "unblock" => named(args, "unblock").map(Cmd::Unblock),
        "remove" => named(args, "remove").map(Cmd::Remove),
        "info" => named(args, "info").map(Cmd::Info),
        "accept" => Ok(Cmd::Accept(optional(args))),
        "block" => Ok(Cmd::Block(optional(args))),
        "name" if args.is_empty() => Err("try: /name <new name>".into()),
        "name" => clean_name(args).map(Cmd::Name).map_err(|e| e.to_string()),
        "phrase" => match words.as_slice() {
            [] => Ok(Cmd::Phrase),
            ["save"] => Ok(Cmd::PhraseSave),
            ["copy"] => Ok(Cmd::PhraseCopy),
            _ => Err("try: /phrase, /phrase save or /phrase copy".into()),
        },
        "update" => match words.as_slice() {
            [] => Ok(Cmd::Update { yes: false }),
            ["yes" | "y"] => Ok(Cmd::Update { yes: true }),
            _ => Err("try: /update or /update yes".into()),
        },
        other => Err(unknown(other)),
    };
    match cmd {
        Ok(cmd) => Input::Cmd(cmd),
        Err(why) => Input::Wrong(why),
    }
}

fn no_args(args: &str, name: &str, cmd: Cmd) -> Result<Cmd, String> {
    if args.is_empty() { Ok(cmd) } else { Err(format!("try: /{name}")) }
}

fn named(args: &str, name: &str) -> Result<String, String> {
    if args.is_empty() { Err(format!("try: /{name} <name>")) } else { Ok(args.to_string()) }
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
    /// How it's typed, for messages about it.
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
            Cmd::PhraseCopy => "/phrase copy",
            Cmd::Close => "/close",
            Cmd::Clear => "/clear",
            Cmd::Update { .. } => "/update",
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
        for (line, hint) in [
            ("/code now", "try: /code"),
            ("/contacts all", "try: /contacts"),
            ("/requests x", "try: /requests"),
            ("/blocked x", "try: /blocked"),
            ("/passphrase new", "try: /passphrase"),
            ("/close bob", "try: /close"),
            ("/clear all", "try: /clear"),
            ("/help me", "try: /help"),
            ("/q now", "try: /quit"),
        ] {
            assert_eq!(wrong(line), hint, "{line}");
        }
    }

    #[test]
    fn add_takes_a_code_and_checks_it() {
        let code = Seed::generate().code();
        assert_eq!(cmd(&format!("/add {code}")), Cmd::Add(code));
        let text = code.to_string();
        let (a, b) = text.split_at(20);
        assert_eq!(cmd(&format!("/add {a} {b}")), Cmd::Add(code), "pasted with a break in it");
        assert!(wrong("/add").starts_with("try: /add <code>"));
        assert!(wrong(&format!("/add {}", &text[..30])).contains("probably missing"));
        // One character of the key changed: only the checksum can tell.
        let swapped = if &text[10..11] == "a" { "b" } else { "a" };
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
        for (line, hint) in [
            ("/chat", "try: /chat <name>"),
            ("/unblock ", "try: /unblock <name>"),
            ("/remove", "try: /remove <name>"),
            ("/info", "try: /info <name>"),
        ] {
            assert_eq!(wrong(line), hint, "{line}");
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
        assert_eq!(wrong("/name"), "try: /name <new name>");
        assert!(wrong(&format!("/name {}", "x".repeat(33))).contains("33 characters"));
        assert!(wrong("/name a\u{202E}b").contains("can't"));
    }

    #[test]
    fn phrase_and_update_take_a_fixed_word() {
        assert_eq!(cmd("/phrase save"), Cmd::PhraseSave);
        assert_eq!(cmd("/phrase copy"), Cmd::PhraseCopy);
        assert_eq!(wrong("/phrase print"), "try: /phrase, /phrase save or /phrase copy");
        assert_eq!(wrong("/phrase save now"), "try: /phrase, /phrase save or /phrase copy");
        assert_eq!(cmd("/update"), Cmd::Update { yes: false });
        assert_eq!(cmd("/update yes"), Cmd::Update { yes: true });
        assert_eq!(cmd("/update y"), Cmd::Update { yes: true });
        assert_eq!(wrong("/update now"), "try: /update or /update yes");
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
