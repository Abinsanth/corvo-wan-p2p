//! Copying a message you double-click: with the system's own tool, or through
//! the terminal (OSC 52) over SSH or where there's no tool. Nothing else is
//! ever copied.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a tool gets before it's stopped.
const TOOL_TIME: Duration = Duration::from_secs(2);

/// A program that puts what it reads on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// Reads UTF-16 after a byte-order mark, as Windows' `clip` needs for
    /// anything but plain ASCII.
    pub wide: bool,
}

/// How a copy went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// A tool took it.
    ByTool,
    /// Sent through the terminal, which may have ignored it.
    ByTerminal,
}

impl Copied {
    /// What the flash says.
    pub fn said(self) -> &'static str {
        match self {
            Copied::ByTool => "copied",
            Copied::ByTerminal => "sent to your clipboard if your terminal allows it",
        }
    }
}

/// The tools to try, in order, on `os` (as `std::env::consts::OS` says);
/// `set` says whether an environment variable is set. None over SSH, where
/// they'd copy on the far machine.
pub fn tools(os: &str, set: impl Fn(&str) -> bool) -> Vec<Tool> {
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return Vec::new();
    }
    let tool = |program, args, wide| Tool { program, args, wide };
    match os {
        "macos" => vec![tool("pbcopy", &[], false)],
        "windows" => vec![tool("clip", &[], true)],
        _ => {
            let mut tools = Vec::new();
            if set("WAYLAND_DISPLAY") {
                tools.push(tool("wl-copy", &[], false));
            }
            if set("DISPLAY") {
                tools.push(tool("xclip", &["-selection", "clipboard"], false));
                tools.push(tool("xsel", &["--clipboard", "--input"], false));
            }
            tools
        }
    }
}

/// Gives `text` to `tool` and waits for it to finish.
pub fn run(tool: Tool, text: &str) -> io::Result<()> {
    let mut child = Command::new(tool.program)
        .args(tool.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let written = stdin.write_all(&bytes(tool, text));
    drop(stdin);
    let deadline = Instant::now() + TOOL_TIME;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} took too long", tool.program)));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    written?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("{} failed: {status}", tool.program))) }
}

/// What `tool` reads for `text`.
fn bytes(tool: Tool, text: &str) -> Vec<u8> {
    if tool.wide {
        [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// What puts `text` on the clipboard through the terminal, if it allows it.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(os: &str, vars: &[&str]) -> Vec<&'static str> {
        tools(os, |name| vars.contains(&name)).into_iter().map(|t| t.program).collect()
    }

    #[test]
    fn each_system_has_its_tool_and_ssh_has_none() {
        assert_eq!(programs("macos", &[]), ["pbcopy"]);
        assert_eq!(programs("windows", &[]), ["clip"]);
        assert_eq!(programs("linux", &["WAYLAND_DISPLAY", "DISPLAY"]), ["wl-copy", "xclip", "xsel"]);
        assert_eq!(programs("linux", &["DISPLAY"]), ["xclip", "xsel"]);
        assert!(programs("linux", &[]).is_empty(), "no display: through the terminal");
        assert!(programs("macos", &["SSH_TTY"]).is_empty(), "over SSH: through the terminal");
        assert!(programs("linux", &["SSH_CONNECTION", "DISPLAY"]).is_empty());
    }

    #[test]
    fn clip_reads_utf16_with_a_byte_order_mark() {
        let clip = tools("windows", |_| false)[0];
        assert_eq!(bytes(clip, "hé"), [0xFF, 0xFE, b'h', 0, 0xE9, 0]);
        let pbcopy = tools("macos", |_| false)[0];
        assert_eq!(bytes(pbcopy, "hé"), "hé".as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_reads_the_text_and_a_failing_one_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("clipboard");
        let path: &'static str = String::leak(file.to_string_lossy().into_owned());
        let cat = Tool { program: "sh", args: Vec::leak(vec!["-c", "cat > \"$0\"", path]), wide: false };
        run(cat, "two\nlines").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\nlines");
        assert!(run(Tool { program: "false", args: &[], wide: false }, "x").is_err());
        assert!(run(Tool { program: "no-such-copy-tool", args: &[], wide: false }, "x").is_err());
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn osc52_wraps_the_text_for_the_clipboard() {
        assert_eq!(osc52("corvo1abc"), "\x1b]52;c;Y29ydm8xYWJj\x07");
    }
}
