use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct NormalizeReport {
    pub zero_width: Option<usize>,
    pub control_chars: Option<usize>,
    pub trailing_spaces: Option<usize>,
    pub missing_final_newline: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    pub strip_zero_width: bool,
    pub strip_control: bool,
    pub trim_trailing_space: bool,
    pub ensure_eol: bool,
    pub detect_zero_width: bool,
    pub detect_control: bool,
    pub detect_trailing_space: bool,
    pub detect_final_newline: bool,
}

pub struct NormalizeOutcome {
    pub report: NormalizeReport,
    pub cleaned: Option<String>,
}

pub fn normalize_text(text: &str, opts: &NormalizeOptions) -> NormalizeOutcome {
    let mut zero_width = opts.detect_zero_width.then_some(0usize);
    let mut control_chars = opts.detect_control.then_some(0usize);
    let mut trailing_spaces = opts.detect_trailing_space.then_some(0usize);
    let missing_final_newline = opts
        .detect_final_newline
        .then_some(!text.is_empty() && !text.ends_with('\n'));

    let mut cleaned = String::with_capacity(text.len());
    let mut line_buffer = String::new();
    let mut changed = false;

    for ch in text.chars() {
        if ch == '\n' {
            flush_line(
                &mut line_buffer,
                &mut cleaned,
                opts,
                trailing_spaces.as_mut(),
                &mut changed,
                true,
            );
            continue;
        }

        if is_zero_width_char(ch) {
            if let Some(count) = zero_width.as_mut() {
                *count += 1;
            }
            if opts.strip_zero_width {
                changed = true;
                continue;
            }
        }

        if is_control_char(ch) {
            if let Some(count) = control_chars.as_mut() {
                *count += 1;
            }
            if opts.strip_control {
                changed = true;
                continue;
            }
        }

        line_buffer.push(ch);
    }

    flush_line(
        &mut line_buffer,
        &mut cleaned,
        opts,
        trailing_spaces.as_mut(),
        &mut changed,
        false,
    );

    if opts.ensure_eol && !cleaned.ends_with('\n') {
        cleaned.push('\n');
        changed = true;
    }

    let report = NormalizeReport {
        zero_width,
        control_chars,
        trailing_spaces,
        missing_final_newline,
    };

    if changed || cleaned != text {
        NormalizeOutcome {
            report,
            cleaned: Some(cleaned),
        }
    } else {
        NormalizeOutcome {
            report,
            cleaned: None,
        }
    }
}

fn is_zero_width_char(ch: char) -> bool {
    matches!(ch, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')
}

fn is_control_char(ch: char) -> bool {
    ch.is_control() && ch != '\n' && ch != '\t' && ch != '\r'
}

fn count_trailing_ws(line: &str) -> usize {
    line.chars()
        .rev()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count()
}

fn flush_line(
    line_buffer: &mut String,
    cleaned: &mut String,
    opts: &NormalizeOptions,
    trailing_spaces: Option<&mut usize>,
    changed: &mut bool,
    append_newline: bool,
) {
    let mut had_cr = false;
    if line_buffer.ends_with('\r') {
        line_buffer.pop();
        had_cr = true;
    }

    if opts.detect_trailing_space || opts.trim_trailing_space {
        let trailing = count_trailing_ws(line_buffer);
        if let Some(count) = trailing_spaces {
            *count += trailing;
        }
        if opts.trim_trailing_space && trailing > 0 {
            let new_len = line_buffer.len().saturating_sub(trailing);
            line_buffer.truncate(new_len);
            *changed = true;
        }
    }

    if had_cr {
        line_buffer.push('\r');
    }

    if !line_buffer.is_empty() || append_newline {
        cleaned.push_str(line_buffer);
        if append_newline {
            cleaned.push('\n');
        }
    }
    line_buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_opts() -> NormalizeOptions {
        NormalizeOptions {
            strip_zero_width: false,
            strip_control: false,
            trim_trailing_space: false,
            ensure_eol: false,
            detect_zero_width: true,
            detect_control: true,
            detect_trailing_space: true,
            detect_final_newline: true,
        }
    }

    #[test]
    fn detection_can_be_disabled() {
        let mut opts = base_opts();
        opts.detect_zero_width = false;
        let report = normalize_text("a\u{200B}b", &opts).report;
        assert!(report.zero_width.is_none());
    }

    #[test]
    fn missing_final_newline_reported() {
        let report = normalize_text("no newline", &base_opts()).report;
        assert_eq!(report.missing_final_newline, Some(true));
    }

    #[test]
    fn trim_trailing_space_handles_crlf() {
        let mut opts = base_opts();
        opts.trim_trailing_space = true;
        let outcome = normalize_text("hello  \r\nworld\t \r\n", &opts);
        assert_eq!(outcome.cleaned, Some("hello\r\nworld\r\n".to_string()));
    }
}
