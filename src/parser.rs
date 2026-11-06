use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b(?:\[[0-9;?]*[A-Za-z]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
        .expect("valid regex")
});

static LAUNCH_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\d+\.\d+\s+)?\[(?P<sev>DEBUG|INFO|WARNING|WARN|ERROR|FATAL|CRITICAL)\] \[(?P<actor>[^\]]+)\]: (?P<body>.*)$",
    )
    .expect("valid regex")
});

static CHILD_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\d+\.\d+\s+)?\[(?P<proc>[^\[\]]+-\d+)\] (?P<rest>.*)$")
        .expect("valid regex")
});

static RCUTILS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\[(?P<sev>DEBUG|INFO|WARN|WARNING|ERROR|FATAL|CRITICAL)\] (?:\[\d+(?:\.\d+)?\] )?\[(?P<logger>[^\]]+)\]: (?P<msg>.*)$",
    )
    .expect("valid regex")
});

static CONTINUATION_STACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#\d+\s").expect("valid regex")
});

static PID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"process started with pid \[(\d+)\]").expect("valid regex")
});

static EXIT_CODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"exit code (-?\d+)").expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Unknown,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Severity {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    Started { pid: Option<u32> },
    Exited { code: Option<i32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Launch {
        process: String,
        severity: Severity,
        lifecycle: Option<LifecycleEvent>,
    },
    Prefixed {
        process: String,
        logger: Option<String>,
        severity: Severity,
        display_offset: usize,
        is_continuation: bool,
    },
    Unprefixed {
        logger: Option<String>,
        severity: Severity,
        is_continuation: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLine {
    pub plain: String,
    pub kind: LineKind,
}

#[must_use]
pub fn strip_ansi(input: &str) -> Cow<'_, str> {
    if !input.contains('\x1b') {
        Cow::Borrowed(input)
    } else {
        ANSI_RE.replace_all(input, "")
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "DEBUG" => Severity::Debug,
        "INFO" => Severity::Info,
        "WARN" | "WARNING" => Severity::Warn,
        "ERROR" => Severity::Error,
        "FATAL" | "CRITICAL" => Severity::Fatal,
        _ => Severity::Unknown,
    }
}

fn ends_with_dash_digits(s: &str) -> bool {
    s.rsplit_once('-').is_some_and(|(_, suffix)| {
        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
    })
}

fn parse_lifecycle(body: &str) -> Option<LifecycleEvent> {
    if body.contains("process started with pid [") {
        let pid = PID_RE
            .captures(body)
            .and_then(|c| c.get(1)?.as_str().parse::<u32>().ok());
        Some(LifecycleEvent::Started { pid })
    } else if body.trim_start().starts_with("process has finished cleanly") {
        Some(LifecycleEvent::Exited { code: Some(0) })
    } else if body.trim_start().starts_with("process has died [") {
        let code = EXIT_CODE_RE
            .captures(body)
            .and_then(|c| c.get(1)?.as_str().parse::<i32>().ok());
        Some(LifecycleEvent::Exited { code })
    } else {
        None
    }
}

fn is_continuation_line(rest: &str) -> bool {
    rest.starts_with(|c: char| c.is_ascii_whitespace())
        || rest.starts_with("Traceback (most recent call last):")
        || rest.starts_with("Caused by:")
        || rest.starts_with("File \"")
        || rest.starts_with("at ")
        || CONTINUATION_STACK_RE.is_match(rest)
}

fn compute_display_offset(raw: &str, proc: &str) -> usize {
    let needle = format!("[{proc}] ");
    if let Some(pos) = raw.find(&needle) {
        let prefix_raw = &raw[..pos];
        let prefix_plain = strip_ansi(prefix_raw);
        let trimmed = prefix_plain.trim();
        let is_valid_leading = trimmed.is_empty()
            || (trimmed.chars().all(|c| c.is_ascii_digit() || c == '.')
                && trimmed.parse::<f64>().is_ok());
        if is_valid_leading {
            let offset = pos + needle.len();
            if raw.is_char_boundary(offset) {
                return offset;
            }
        }
    }
    0
}

fn parse_kind(raw: &str, plain: &str) -> LineKind {
    // Precedence 1: ros2 launch own & lifecycle lines
    if let Some(caps) = LAUNCH_LINE_RE.captures(plain) {
        let actor = caps.name("actor").map_or("", |m| m.as_str());
        let sev = caps.name("sev").map_or("", |m| m.as_str());
        let body = caps.name("body").map_or("", |m| m.as_str());

        if actor == "launch" {
            return LineKind::Launch {
                process: "launch".to_string(),
                severity: parse_severity(sev),
                lifecycle: None,
            };
        } else if ends_with_dash_digits(actor) {
            let lifecycle = parse_lifecycle(body);
            return LineKind::Launch {
                process: actor.to_string(),
                severity: parse_severity(sev),
                lifecycle,
            };
        }
    }

    // Precedence 2: ExecuteLocal child-prefixed lines
    if let Some(caps) = CHILD_PREFIX_RE.captures(plain) {
        let proc = caps.name("proc").map_or("", |m| m.as_str());
        let rest = caps.name("rest").map_or("", |m| m.as_str());
        let display_offset = compute_display_offset(raw, proc);

        if let Some(rc_caps) = RCUTILS_RE.captures(rest) {
            let sev = rc_caps.name("sev").map_or("", |m| m.as_str());
            let logger = rc_caps.name("logger").map_or("", |m| m.as_str());
            return LineKind::Prefixed {
                process: proc.to_string(),
                logger: Some(logger.to_string()),
                severity: parse_severity(sev),
                display_offset,
                is_continuation: false,
            };
        } else if is_continuation_line(rest) {
            return LineKind::Prefixed {
                process: proc.to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset,
                is_continuation: true,
            };
        } else {
            return LineKind::Prefixed {
                process: proc.to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset,
                is_continuation: false,
            };
        }
    }

    // Precedence 3: Unprefixed lines
    if let Some(rc_caps) = RCUTILS_RE.captures(plain) {
        let sev = rc_caps.name("sev").map_or("", |m| m.as_str());
        let logger = rc_caps.name("logger").map_or("", |m| m.as_str());
        LineKind::Unprefixed {
            logger: Some(logger.to_string()),
            severity: parse_severity(sev),
            is_continuation: false,
        }
    } else if is_continuation_line(plain) {
        LineKind::Unprefixed {
            logger: None,
            severity: Severity::Unknown,
            is_continuation: true,
        }
    } else {
        LineKind::Unprefixed {
            logger: None,
            severity: Severity::Unknown,
            is_continuation: false,
        }
    }
}

#[must_use]
pub fn parse_line(raw: &str) -> ParsedLine {
    let plain_cow = strip_ansi(raw);
    let kind = parse_kind(raw, plain_cow.as_ref());
    ParsedLine {
        plain: plain_cow.into_owned(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_own_line() {
        let raw = "1793679402.094 [INFO] [launch]: Default logging verbosity is set to INFO";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "launch".to_string(),
                    severity: Severity::Info,
                    lifecycle: None,
                },
            }
        );
    }

    #[test]
    fn test_lifecycle_process_started() {
        let raw = "[INFO] [robot_state_publisher-1]: process started with pid [3114445]";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "robot_state_publisher-1".to_string(),
                    severity: Severity::Info,
                    lifecycle: Some(LifecycleEvent::Started {
                        pid: Some(3114445),
                    }),
                },
            }
        );
    }

    #[test]
    fn test_lifecycle_process_died() {
        let raw = "[ERROR] [rviz2-3]: process has died [pid 3114447, exit code -9, cmd '...'].";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "rviz2-3".to_string(),
                    severity: Severity::Error,
                    lifecycle: Some(LifecycleEvent::Exited { code: Some(-9) }),
                },
            }
        );
    }

    #[test]
    fn test_lifecycle_process_finished_cleanly() {
        let raw = "[INFO] [nav2_controller-2]: process has finished cleanly";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "nav2_controller-2".to_string(),
                    severity: Severity::Info,
                    lifecycle: Some(LifecycleEvent::Exited { code: Some(0) }),
                },
            }
        );
    }

    #[test]
    fn test_lifecycle_started_unparseable_pid() {
        let raw = "[INFO] [node-1]: process started with pid [unknown]";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "node-1".to_string(),
                    severity: Severity::Info,
                    lifecycle: Some(LifecycleEvent::Started { pid: None }),
                },
            }
        );
    }

    #[test]
    fn test_lifecycle_died_missing_code() {
        let raw = "[ERROR] [node-1]: process has died [cmd 'crash']";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Launch {
                    process: "node-1".to_string(),
                    severity: Severity::Error,
                    lifecycle: Some(LifecycleEvent::Exited { code: None }),
                },
            }
        );
    }

    #[test]
    fn test_execute_local_rcutils_composable() {
        let raw = "[component_container_mt-1] [WARN] [1793679405.123] [controller_server]: Goal slow";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed,
            ParsedLine {
                plain: raw.to_string(),
                kind: LineKind::Prefixed {
                    process: "component_container_mt-1".to_string(),
                    logger: Some("controller_server".to_string()),
                    severity: Severity::Warn,
                    display_offset: 27,
                    is_continuation: false,
                },
            }
        );
        assert_eq!(
            &raw[27..],
            "[WARN] [1793679405.123] [controller_server]: Goal slow"
        );
    }

    #[test]
    fn parse_line_preserves_child_ansi_and_char_boundary_display_offset() {
        let raw = "\x1b[0m[node_café-1] \x1b[33m[WARN] [my_node]: café\x1b[0m";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed.plain,
            "[node_café-1] [WARN] [my_node]: café"
        );
        assert_eq!(
            parsed.kind,
            LineKind::Prefixed {
                process: "node_café-1".to_string(),
                logger: Some("my_node".to_string()),
                severity: Severity::Warn,
                display_offset: 19,
                is_continuation: false,
            }
        );
        assert_eq!(
            &raw[19..],
            "\x1b[33m[WARN] [my_node]: café\x1b[0m"
        );
    }

    #[test]
    fn test_prefixed_continuation_and_raw_output() {
        let line1 = "[node-1]   File \"foo.py\", line 10";
        let p1 = parse_line(line1);
        assert_eq!(
            p1.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line2 = "[node-1] Traceback (most recent call last):";
        let p2 = parse_line(line2);
        assert_eq!(
            p2.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line3 = "[node-1] Caused by: java.lang.Exception";
        let p3 = parse_line(line3);
        assert_eq!(
            p3.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line4 = "[node-1] File \"foo.py\", line 10";
        let p4 = parse_line(line4);
        assert_eq!(
            p4.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line5 = "[node-1] at com.example.MyClass";
        let p5 = parse_line(line5);
        assert_eq!(
            p5.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line6 = "[node-1] #0  0x00007ffff7a12345 in func ()";
        let p6 = parse_line(line6);
        assert_eq!(
            p6.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: true,
            }
        );

        let line7 = "[node-1] Gtk-Message: failed";
        let p7 = parse_line(line7);
        assert_eq!(
            p7.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 9,
                is_continuation: false,
            }
        );
    }

    #[test]
    fn test_unprefixed_rcutils_and_raw() {
        let r1 = "[INFO] [1700.1] [standalone]: ready";
        let p1 = parse_line(r1);
        assert_eq!(
            p1.kind,
            LineKind::Unprefixed {
                logger: Some("standalone".to_string()),
                severity: Severity::Info,
                is_continuation: false,
            }
        );

        let r2 = "[INFO] [standalone]: ready";
        let p2 = parse_line(r2);
        assert_eq!(
            p2.kind,
            LineKind::Unprefixed {
                logger: Some("standalone".to_string()),
                severity: Severity::Info,
                is_continuation: false,
            }
        );

        let r3 = "  continuation line with whitespace";
        let p3 = parse_line(r3);
        assert_eq!(
            p3.kind,
            LineKind::Unprefixed {
                logger: None,
                severity: Severity::Unknown,
                is_continuation: true,
            }
        );

        let r4 = "plain raw text";
        let p4 = parse_line(r4);
        assert_eq!(
            p4.kind,
            LineKind::Unprefixed {
                logger: None,
                severity: Severity::Unknown,
                is_continuation: false,
            }
        );
    }

    #[test]
    fn test_strip_ansi() {
        let no_ansi = "hello world";
        assert!(matches!(strip_ansi(no_ansi), Cow::Borrowed(_)));
        assert_eq!(strip_ansi(no_ansi), "hello world");

        let csi = "\x1b[31;1mred bold\x1b[0m";
        assert_eq!(strip_ansi(csi), "red bold");

        let osc_bell = "\x1b]0;Window Title\x07Text";
        assert_eq!(strip_ansi(osc_bell), "Text");

        let osc_slash = "\x1b]0;Window Title\x1b\\Text";
        assert_eq!(strip_ansi(osc_slash), "Text");

        let mixed = "\x1b[0m[node_café-1] \x1b[33m[WARN]\x1b[0m";
        assert_eq!(strip_ansi(mixed), "[node_café-1] [WARN]");
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Unknown < Severity::Debug);
        assert!(Severity::Debug < Severity::Info);
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Error);
        assert!(Severity::Error < Severity::Fatal);
    }

    #[test]
    fn test_display_offset_with_epoch_float() {
        let raw = "1793679405.123 [node-1] hello";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: None,
                severity: Severity::Unknown,
                display_offset: 24,
                is_continuation: false,
            }
        );
        assert_eq!(&raw[24..], "hello");
    }

    #[test]
    fn test_display_offset_fallback_on_invalid() {
        // If needle is not found or char boundary is somehow invalid
        let offset = compute_display_offset("invalid line", "node-1");
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_critical_severity_in_launch_and_rcutils() {
        let launch_crit = "[CRITICAL] [launch]: catastrophic failure";
        let p_launch = parse_line(launch_crit);
        assert_eq!(
            p_launch.kind,
            LineKind::Launch {
                process: "launch".to_string(),
                severity: Severity::Fatal,
                lifecycle: None,
            }
        );

        let rcutils_crit = "[node-1] [CRITICAL] [subsystem]: alert";
        let p_rc = parse_line(rcutils_crit);
        assert_eq!(
            p_rc.kind,
            LineKind::Prefixed {
                process: "node-1".to_string(),
                logger: Some("subsystem".to_string()),
                severity: Severity::Fatal,
                display_offset: 9,
                is_continuation: false,
            }
        );
    }

    #[test]
    fn test_rule1_actor_fallthrough_to_unprefixed() {
        // Actor is not "launch" and does not end with -\d+
        let raw = "[INFO] [standalone_logger]: message here";
        let parsed = parse_line(raw);
        assert_eq!(
            parsed.kind,
            LineKind::Unprefixed {
                logger: Some("standalone_logger".to_string()),
                severity: Severity::Info,
                is_continuation: false,
            }
        );
    }

    #[test]
    fn test_unprefixed_continuation_variants() {
        let lines = [
            ("Caused by: std::runtime_error", true),
            ("File \"/opt/ros/lib/test.py\", line 5", true),
            ("at java.base/java.lang.Thread.run", true),
            ("#0 0x7fff12345 in abort ()", true),
            ("Gtk-Message: not a continuation", false),
        ];

        for (line, expected_cont) in lines {
            let parsed = parse_line(line);
            assert_eq!(
                parsed.kind,
                LineKind::Unprefixed {
                    logger: None,
                    severity: Severity::Unknown,
                    is_continuation: expected_cont,
                },
                "failed for line: {line}"
            );
        }
    }

    #[test]
    fn test_severity_as_str() {
        assert_eq!(Severity::Unknown.as_str(), "UNKNOWN");
        assert_eq!(Severity::Debug.as_str(), "DEBUG");
        assert_eq!(Severity::Info.as_str(), "INFO");
        assert_eq!(Severity::Warn.as_str(), "WARN");
        assert_eq!(Severity::Error.as_str(), "ERROR");
        assert_eq!(Severity::Fatal.as_str(), "FATAL");
    }

    #[test]
    fn test_prefixed_with_ansi_around_timestamp() {
        let raw = "\x1b[32m1793679405.123\x1b[0m [node-1] hello";
        let parsed = parse_line(raw);
        let offset = match parsed.kind {
            LineKind::Prefixed { display_offset, .. } => display_offset,
            _ => panic!("expected Prefixed"),
        };
        assert_eq!(&raw[offset..], "hello");
    }
}
