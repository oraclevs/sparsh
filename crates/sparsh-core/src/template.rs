//! Right-prompt slot templates: literal text mixed with `{widget}` /
//! `{widget:args}` placeholders. Templates are parsed and validated once at
//! config load; a parse error breaks only the slot that contains it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WidgetKind {
    Time,
    Date,
    Duration,
    Cpu,
    Ram,
    Disk,
    Battery,
    Load,
    Uptime,
    User,
    Host,
    Jobs,
}

impl WidgetKind {
    pub const ALL: [WidgetKind; 12] = [
        WidgetKind::Time,
        WidgetKind::Date,
        WidgetKind::Duration,
        WidgetKind::Cpu,
        WidgetKind::Ram,
        WidgetKind::Disk,
        WidgetKind::Battery,
        WidgetKind::Load,
        WidgetKind::Uptime,
        WidgetKind::User,
        WidgetKind::Host,
        WidgetKind::Jobs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            WidgetKind::Time => "time",
            WidgetKind::Date => "date",
            WidgetKind::Duration => "duration",
            WidgetKind::Cpu => "cpu",
            WidgetKind::Ram => "ram",
            WidgetKind::Disk => "disk",
            WidgetKind::Battery => "battery",
            WidgetKind::Load => "load",
            WidgetKind::Uptime => "uptime",
            WidgetKind::User => "user",
            WidgetKind::Host => "host",
            WidgetKind::Jobs => "jobs",
        }
    }

    pub fn from_name(name: &str) -> Option<WidgetKind> {
        WidgetKind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetRef {
    pub kind: WidgetKind,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Literal(String),
    Widget(WidgetRef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub pieces: Vec<Piece>,
}

const SIZE_MODES: [&str; 4] = ["free", "used", "avail", "total"];

impl Template {
    pub fn parse(source: &str) -> Result<Template, String> {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        let mut chars = source.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => return Err("unmatched '}' (write '}}' for a literal brace)".into()),
                '{' => {
                    let mut body = String::new();
                    let mut closed = false;
                    for next in chars.by_ref() {
                        if next == '}' {
                            closed = true;
                            break;
                        }
                        body.push(next);
                    }
                    if !closed {
                        return Err("unclosed '{' (write '{{' for a literal brace)".into());
                    }
                    if !literal.is_empty() {
                        pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                    }
                    pieces.push(Piece::Widget(parse_placeholder(&body)?));
                }
                c if c.is_control() => return Err("control character in text".into()),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            pieces.push(Piece::Literal(literal));
        }
        Ok(Template { pieces })
    }

    pub fn widgets(&self) -> impl Iterator<Item = &WidgetRef> {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Widget(widget) => Some(widget),
            Piece::Literal(_) => None,
        })
    }

    pub fn has_widgets(&self) -> bool {
        self.widgets().next().is_some()
    }
}

fn parse_placeholder(body: &str) -> Result<WidgetRef, String> {
    let (name, raw_args) = match body.split_once(':') {
        Some((name, args)) => (name, Some(args)),
        None => (body, None),
    };
    if name.is_empty() {
        return Err("empty placeholder '{}'".into());
    }
    let Some(kind) = WidgetKind::from_name(name) else {
        let names: Vec<&str> = WidgetKind::ALL.iter().map(|kind| kind.name()).collect();
        return Err(match suggest(name, &names) {
            Some(close) => format!("unknown widget '{name}'; did you mean '{close}'?"),
            None => format!(
                "unknown widget '{name}' (expected one of: {})",
                names.join(", ")
            ),
        });
    };

    let args: Vec<String> = match (kind, raw_args) {
        (_, None) => Vec::new(),
        // strftime formats may contain commas, so time/date keep the raw text.
        (WidgetKind::Time | WidgetKind::Date, Some(raw)) => vec![raw.to_string()],
        (_, Some(raw)) => raw.split(',').map(|arg| arg.trim().to_string()).collect(),
    };
    validate_args(kind, &args)?;
    Ok(WidgetRef { kind, args })
}

fn validate_args(kind: WidgetKind, args: &[String]) -> Result<(), String> {
    let name = kind.name();
    let unknown_mode = |mode: &str, expected: &str| {
        format!("unknown mode '{mode}' for widget '{name}' (expected {expected})")
    };
    match kind {
        WidgetKind::Time | WidgetKind::Date => {
            if let Some(format) = args.first() {
                if format.is_empty() {
                    return Err(format!("widget '{name}' has an empty format"));
                }
                if format.chars().any(char::is_control) {
                    return Err(format!(
                        "widget '{name}' format contains a control character"
                    ));
                }
            }
        }
        WidgetKind::Duration | WidgetKind::Uptime | WidgetKind::User | WidgetKind::Jobs => {
            if !args.is_empty() {
                return Err(format!("widget '{name}' does not take arguments"));
            }
        }
        WidgetKind::Cpu => {
            for arg in args {
                if arg != "free" {
                    return Err(unknown_mode(arg, "free"));
                }
            }
            if args.len() > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
        }
        WidgetKind::Ram => {
            if args.len() > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
            for arg in args {
                if !SIZE_MODES.contains(&arg.as_str()) {
                    return Err(unknown_mode(arg, "free, used, avail or total"));
                }
            }
        }
        WidgetKind::Disk => {
            let mut modes = 0;
            let mut paths = 0;
            for arg in args {
                if arg.starts_with('/') {
                    paths += 1;
                } else if SIZE_MODES.contains(&arg.as_str()) {
                    modes += 1;
                } else {
                    return Err(unknown_mode(
                        arg,
                        "free, used, avail, total or an absolute path",
                    ));
                }
            }
            if modes > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
            if paths > 1 {
                return Err(format!("widget '{name}' takes at most one path"));
            }
        }
        WidgetKind::Battery => {
            if args.len() > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
            for arg in args {
                if arg != "icon" && arg != "state" {
                    return Err(unknown_mode(arg, "icon or state"));
                }
            }
        }
        WidgetKind::Load => {
            if args.len() > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
            for arg in args {
                if !["1", "5", "15"].contains(&arg.as_str()) {
                    return Err(unknown_mode(arg, "1, 5 or 15"));
                }
            }
        }
        WidgetKind::Host => {
            if args.len() > 1 {
                return Err(format!("widget '{name}' takes at most one mode"));
            }
            for arg in args {
                if arg != "full" {
                    return Err(unknown_mode(arg, "full"));
                }
            }
        }
    }
    Ok(())
}

/// The closest candidate within an edit distance of 2 (and at most half the
/// name's length), for "did you mean" hints.
pub fn suggest(name: &str, candidates: &[&str]) -> Option<String> {
    let limit = 2.min(name.chars().count().max(1) / 2 + 1);
    candidates
        .iter()
        .map(|candidate| (edit_distance(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Piece {
        Piece::Literal(s.into())
    }

    fn w(kind: WidgetKind, args: &[&str]) -> Piece {
        Piece::Widget(WidgetRef {
            kind,
            args: args.iter().map(|a| a.to_string()).collect(),
        })
    }

    #[test]
    fn parses_literals_widgets_and_escaped_braces() {
        let t = Template::parse("  {cpu}% {{x}} {time:%H:%M}").unwrap();
        assert_eq!(
            t.pieces,
            vec![
                lit("  "),
                w(WidgetKind::Cpu, &[]),
                lit("% {x} "),
                w(WidgetKind::Time, &["%H:%M"])
            ]
        );
        assert!(t.has_widgets());
        assert!(!Template::parse("just text \u{f303}").unwrap().has_widgets());
    }

    #[test]
    fn time_and_date_keep_commas_in_the_format() {
        let t = Template::parse("{date:%a, %d %b}").unwrap();
        assert_eq!(t.pieces, vec![w(WidgetKind::Date, &["%a, %d %b"])]);
    }

    #[test]
    fn other_widgets_split_args_on_commas() {
        let t = Template::parse("{disk:/home,free}").unwrap();
        assert_eq!(t.pieces, vec![w(WidgetKind::Disk, &["/home", "free"])]);
    }

    #[test]
    fn unknown_widget_suggests_the_closest_name() {
        let e = Template::parse("{cpuu}").unwrap_err();
        assert!(
            e.contains("unknown widget 'cpuu'") && e.contains("did you mean 'cpu'?"),
            "{e}"
        );
        let e = Template::parse("{zzzzzz}").unwrap_err();
        assert!(
            e.contains("unknown widget 'zzzzzz'") && !e.contains("did you mean"),
            "{e}"
        );
    }

    #[test]
    fn structural_errors_are_reported() {
        assert!(Template::parse("{cpu")
            .unwrap_err()
            .contains("unclosed '{'"));
        assert!(Template::parse("cpu}")
            .unwrap_err()
            .contains("unmatched '}'"));
        assert!(Template::parse("{}")
            .unwrap_err()
            .contains("empty placeholder"));
    }

    #[test]
    fn argument_validation_per_widget() {
        assert!(Template::parse("{ram:used}").is_ok());
        assert!(Template::parse("{ram:bogus}")
            .unwrap_err()
            .contains("unknown mode 'bogus' for widget 'ram'"));
        assert!(Template::parse("{duration:x}")
            .unwrap_err()
            .contains("does not take arguments"));
        assert!(Template::parse("{disk:free,used}")
            .unwrap_err()
            .contains("at most one mode"));
        assert!(Template::parse("{disk:/a,/b}")
            .unwrap_err()
            .contains("at most one path"));
        assert!(Template::parse("{battery:icon}").is_ok());
        assert!(Template::parse("{load:5}").is_ok());
        assert!(Template::parse("{load:7}").is_err());
        assert!(Template::parse("{host:full}").is_ok());
        assert!(Template::parse("{time:}")
            .unwrap_err()
            .contains("empty format"));
        assert!(Template::parse("{time:a\u{7}b}")
            .unwrap_err()
            .contains("control character"));
    }

    #[test]
    fn suggest_finds_close_names_only() {
        assert_eq!(
            suggest("batery", &["battery", "cpu"]),
            Some("battery".into())
        );
        assert_eq!(suggest("xyz", &["battery", "cpu"]), None);
    }
}
