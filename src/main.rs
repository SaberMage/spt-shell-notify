//! `notify-shell` — the OS-notification shell binary for spt-core.
//!
//! **Standalone by design**: this project's manifest + this binary are the
//! only glue to spt-core. The binary speaks nothing but the public `spt api`
//! command surface and the documented EVENT envelope wire format (decoded
//! here by hand — no spt-core crate dependency).
//!
//! Three modes:
//!
//! 1. **Resident shell** (the manifest's `spawn` template): broker-launched
//!    with `--link <token> --id <id>` — runs the `api bind-shell --link`
//!    handshake (onlines the perch), then drains `api poll <id> --link`
//!    resident, parsing each `shell_command` frame and rendering
//!    `op="notify"` as a native OS notification. Teardown's kill ends it.
//! 2. **One-shot render** (the manifest's `[session.notif]` template):
//!    `--render-title <t> --render-body <b>` — render once and exit. spt-core
//!    spawns this detached whenever a subnet notif surfaces at an endpoint
//!    this shell is attached to.
//! 3. **Wake watcher** (the manifest's `wake_command`): `--wake` — the
//!    display is assumed always-ready, so settle briefly and exit with the
//!    wake opcode (86).
//!
//! Renders: Windows toast (WinRT via a spawned PowerShell — no crate
//! dependency, headless-safe) / Linux `notify-send`. A failed render is a
//! logged diagnostic, never an exit. `--render-file <path>` appends
//! `title␟body` lines instead of touching the OS — the E2E observable.
//!
//! Frames arrive MAC-stamped (`<mac> <frame>`); the drain itself is already
//! link-token-authenticated (`api poll --link` refuses a wrong token), so the
//! stamp is stripped, not re-verified — at-rest spool tampering is the
//! daemon's trust domain, not the display's.

use std::process::Command;
use std::time::Duration;

/// The spt-core wake opcode: a `wake_command` exiting with this asks the
/// watcher to revive the shell.
const WAKE_OPCODE: i32 = 86;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn required(args: &[String], flag: &str) -> String {
    arg(args, flag).unwrap_or_else(|| {
        eprintln!("notify-shell: {flag} required");
        std::process::exit(2);
    })
}

/// An `spt` invocation that never surfaces a console window: this binary runs
/// detached/console-less, and a console-subsystem child with no console to
/// inherit makes Windows allocate a visible one per poll tick.
fn spt_cmd(spt: &str, args: &[&str]) -> Command {
    let mut c = Command::new(spt);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

// ── EVENT envelope decode (the documented spt-core wire format) ─────────────
//
// `<EVENT type="shell_command" from="<owner>" op="<op>">{json args}</EVENT>`
// Attribute values are HTML-escaped; the body is HTML-escaped with `\n`→`<br>`.
// Decode order is load-bearing: `<br>` first, tag entities, `&amp;`→`&` LAST
// (amp-last — `&amp;lt;` must yield the literal `&lt;`, never double-decode).

fn body_unescape(s: &str) -> String {
    s.replace("<br>", "\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&") // amp-last
}

fn attr_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&") // amp-last
}

/// Pull one attribute's decoded value out of the envelope's open tag.
fn attr_of(open_tag: &str, key: &str) -> Option<String> {
    let marker = format!(" {key}=\"");
    let start = open_tag.find(&marker)? + marker.len();
    let rest = &open_tag[start..];
    let end = rest.find('"')?;
    Some(attr_unescape(&rest[..end]))
}

/// Extract `(title, body)` from one drained spool line: strip the MAC stamp
/// (`<mac> <frame>`), parse the EVENT framing, accept only `shell_command`
/// frames with `op="notify"`, and read the named args the owner's vocabulary
/// check composed. `None` = not a notify command (text/file frames, foreign
/// ops, garbage — silently skipped; an unknown frame must not wedge the
/// display).
fn parse_notify(line: &str) -> Option<(String, String)> {
    let (_mac, frame) = line.trim().split_once(' ')?;
    let frame = frame.trim();
    // Framing: <EVENT ...>body</EVENT>, refusing EVENT-PART chunks.
    let after_open = frame.strip_prefix("<EVENT")?;
    if after_open.starts_with('-') {
        return None;
    }
    let tag_end = after_open.find('>')?;
    let open_tag = &after_open[..tag_end];
    let rest = &after_open[tag_end + 1..];
    let body_escaped = rest.strip_suffix("</EVENT>")?;

    if attr_of(open_tag, "type").as_deref() != Some("shell_command")
        || attr_of(open_tag, "op").as_deref() != Some("notify")
    {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(&body_unescape(body_escaped)).ok()?;
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("spt").to_string();
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    Some((title, body))
}

// ── Native render ────────────────────────────────────────────────────────────

/// The PowerShell WinRT toast snippet for `(title, body)` — a pure function
/// so the embedding (PS single-quote doubling: the only escape that matters
/// inside `'…'`) is unit-testable without a display.
#[cfg(any(windows, test))]
fn windows_toast_script(title: &str, body: &str) -> String {
    let t = title.replace('\'', "''");
    let b = body.replace('\'', "''");
    format!(
        "$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime];\
         $xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);\
         $texts = $xml.GetElementsByTagName('text');\
         $null = $texts.Item(0).AppendChild($xml.CreateTextNode('{t}'));\
         $null = $texts.Item(1).AppendChild($xml.CreateTextNode('{b}'));\
         $toast = [Windows.UI.Notifications.ToastNotification]::new($xml);\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('spt').Show($toast)"
    )
}

/// Render one notification natively. Errors are returned for the caller's
/// diagnostic — never fatal (a headless host has no display; the shell keeps
/// going).
fn render_os(title: &str, body: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let script = windows_toast_script(title, body);
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = c.output().map_err(|e| format!("spawn powershell: {e}"))?;
        if !out.status.success() {
            return Err(format!("toast: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("notify-send")
            .arg(title)
            .arg(body)
            .output()
            .map_err(|e| format!("spawn notify-send: {e}"))?;
        if !out.status.success() {
            return Err(format!("notify-send: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(())
    }
}

/// Render to the observable file (tests) or the OS (production).
fn render(title: &str, body: &str, render_file: &Option<String>) {
    match render_file {
        Some(path) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{title}\u{1f}{body}");
            }
        }
        None => {
            if let Err(e) = render_os(title, body) {
                eprintln!("notify-shell: render dropped (headless?): {e}");
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Wake watcher mode: the display is always-ready — settle, report wake.
    if args.iter().any(|a| a == "--wake") {
        std::thread::sleep(Duration::from_secs(2));
        std::process::exit(WAKE_OPCODE);
    }

    let render_file = arg(&args, "--render-file");

    // One-shot render mode (the [session.notif] template): render and exit.
    if let Some(title) = arg(&args, "--render-title") {
        let body = arg(&args, "--render-body").unwrap_or_default();
        render(&title, &body, &render_file);
        return;
    }

    // Resident shell mode (the spawn template).
    let link = required(&args, "--link");
    let id = required(&args, "--id");
    let spt = arg(&args, "--spt")
        .or_else(|| std::env::var("SPT_BIN").ok())
        .unwrap_or_else(|| "spt".to_string());

    // 1. The local-link handshake: bind flips the perch online.
    let bind =
        spt_cmd(&spt, &["api", "--adapter", "notify", "bind-shell", "--link", &link]).output();
    if !matches!(&bind, Ok(o) if o.status.success()) {
        eprintln!("notify-shell: api bind-shell failed: {bind:?}");
        std::process::exit(1);
    }

    // 2. Resident relay drain: render every notify command until teardown's
    //    kill ends the binary (a relay shell never exits itself).
    loop {
        let drained =
            match spt_cmd(&spt, &["api", "--adapter", "notify", "poll", &id, "--link", &link])
                .output()
            {
                Ok(out) if out.status.success() => {
                    String::from_utf8_lossy(&out.stdout).into_owned()
                }
                _ => String::new(),
            };
        for line in drained.lines().filter(|l| !l.trim().is_empty()) {
            if let Some((title, body)) = parse_notify(line) {
                render(&title, &body, &render_file);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stamped notify frame as the owner's `shell cmd` spools it (the MAC
    /// prefix is opaque to the parser — any first token).
    fn stamped(op: &str, json_escaped: &str) -> String {
        format!(
            "deadbeef <EVENT type=\"shell_command\" from=\"doyle\" op=\"{op}\">{json_escaped}</EVENT>"
        )
    }

    #[test]
    fn parses_notify_and_skips_everything_else() {
        // The body rides HTML-escaped on the wire (quotes → &quot;).
        let escaped = "{&quot;title&quot;:&quot;Build&quot;,&quot;body&quot;:&quot;tests green&quot;}";
        let (t, b) = parse_notify(&stamped("notify", escaped)).unwrap();
        assert_eq!((t.as_str(), b.as_str()), ("Build", "tests green"));

        // Title-only: body defaults empty (trailing args may be omitted).
        let (t, b) = parse_notify(&stamped("notify", "{&quot;title&quot;:&quot;ping&quot;}")).unwrap();
        assert_eq!((t.as_str(), b.as_str()), ("ping", ""));

        assert!(parse_notify(&stamped("move", "{}")).is_none(), "foreign op");
        assert!(
            parse_notify("deadbeef <EVENT type=\"shell_text\" from=\"d\">hi</EVENT>").is_none(),
            "text frame"
        );
        assert!(
            parse_notify("deadbeef <EVENT-PART seq=\"1/2\" id=\"aa\">x</EVENT-PART>").is_none(),
            "EVENT-PART chunk refused"
        );
        assert!(parse_notify("not a frame at all").is_none(), "garbage");
        assert!(parse_notify("").is_none(), "empty");
    }

    #[test]
    fn body_decode_is_amp_last() {
        // A newline in an arg rides as the JSON `\n` escape (JSON strings
        // never carry raw control chars), and `&amp;lt;` must decode amp-last
        // to the literal `&lt;`, never double-decode into `<`.
        let escaped =
            "{&quot;title&quot;:&quot;a\\nb&quot;,&quot;body&quot;:&quot;&amp;lt;keep&amp;gt;&quot;}";
        let (t, b) = parse_notify(&stamped("notify", escaped)).unwrap();
        assert_eq!(t, "a\nb");
        assert_eq!(b, "&lt;keep&gt;");
    }

    #[test]
    fn windows_toast_script_escapes_single_quotes() {
        let s = windows_toast_script("it's done", "don't; $(Remove-Item x)");
        assert!(s.contains("CreateTextNode('it''s done')"));
        assert!(s.contains("CreateTextNode('don''t; $(Remove-Item x)')"));
        // The dangerous payload stays inside the quoted literal — no bare
        // single quote opens an expression context.
        assert!(!s.contains("'don't"));
    }
}
