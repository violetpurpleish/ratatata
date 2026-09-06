//! Web-link detection and platform-native URL launching.

use super::*;

/// Find a web link containing `char_index` in one editor line.
///
/// Link detection deliberately works on the text rather than syntax
/// highlighting, so it also works in plain-text and source files. Markdown
/// punctuation around a URL is ignored, while balanced parentheses inside a
/// URL are retained. `www.` links are normalized to HTTPS before launching.
pub(super) fn link_at(line: &str, char_index: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    for start in 0..chars.len() {
        let is_http = starts_with_ascii(&chars, start, "http://")
            || starts_with_ascii(&chars, start, "https://");
        let is_www = starts_with_ascii(&chars, start, "www.");
        if !is_http && !is_www {
            continue;
        }
        // Do not treat the middle of an identifier as the start of a link.
        if start > 0 && (chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_') {
            continue;
        }

        let mut end = chars[start..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| start + offset);
        end = trim_link_end(&chars, start, end);
        if start >= end || char_index < start || char_index >= end {
            continue;
        }

        let mut url: String = chars[start..end].iter().collect();
        if is_www {
            url.insert_str(0, "https://");
        }
        return Some(url);
    }
    None
}

/// ASCII case-insensitive prefix matching for URL schemes, whose spelling is
/// case-insensitive even though the rest of a URL is not necessarily so.
fn starts_with_ascii(chars: &[char], start: usize, prefix: &str) -> bool {
    chars
        .get(start..start.saturating_add(prefix.chars().count()))
        .is_some_and(|candidate| {
            candidate
                .iter()
                .zip(prefix.chars())
                .all(|(a, b)| a.eq_ignore_ascii_case(&b))
        })
}

/// Remove punctuation commonly placed after a URL in prose or Markdown.
fn trim_link_end(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start
        && matches!(
            chars[end - 1],
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"'
        )
    {
        end -= 1;
    }
    while let Some(&closing) = chars.get(end.saturating_sub(1)) {
        let opening = match closing {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        let opens = chars[start..end].iter().filter(|&&c| c == opening).count();
        let closes = chars[start..end].iter().filter(|&&c| c == closing).count();
        if closes > opens {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// Launch a URL using the operating system's default browser.
pub(super) fn open_in_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        if url.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "URL contains NUL",
            ));
        }
        let url: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // ShellExecuteW opens the URL as data; no command interpreter parses it.
        // SAFETY: url is NUL-terminated and lives throughout the call; optional
        // arguments and the owner window are null as permitted by this API.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result <= 32 {
            Err(io::Error::other(format!(
                "cannot open URL (ShellExecuteW error {result})"
            )))
        } else {
            Ok(())
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = url;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no browser launcher for this platform",
        ))
    }
}
