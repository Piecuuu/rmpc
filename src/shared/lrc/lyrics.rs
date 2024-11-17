use std::{str::FromStr, time::Duration};

use anyhow::{bail, Context, Result};

use super::parse_length;

#[derive(Debug, Eq, PartialEq)]
pub struct LrcLine {
    pub time: Duration,
    pub content: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Lrc {
    pub lines: Vec<LrcLine>,
    /// ti
    pub title: Option<String>,
    /// ar
    pub artist: Option<String>,
    /// al
    pub album: Option<String>,
    /// au
    pub author: Option<String>,
    /// length
    pub length: Option<Duration>,
}

impl FromStr for Lrc {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut offset: Option<i64> = None;
        let mut result = Self {
            lines: Vec::new(),
            title: None,
            artist: None,
            album: None,
            author: None,
            length: None,
        };

        for s in s.lines() {
            if s.is_empty() || s.starts_with('#') {
                continue;
            }

            let (meta_or_time, line) = s
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.split_once(']'))
                .with_context(|| format!("Invalid lrc line format: '{s}'"))?;

            match meta_or_time.chars().next() {
                Some(c) if c.is_numeric() => {
                    let (minutes, time_rest) = meta_or_time
                        .split_once(':')
                        .with_context(|| format!("Invalid lrc minutes format: '{meta_or_time}'"))?;
                    let (seconds, hundreths) = time_rest
                        .split_once('.')
                        .or_else(|| time_rest.split_once(':'))
                        .with_context(|| format!("Invalid lrc seconds and hundreths format: '{time_rest}'"))?;

                    let mut milis = 0;
                    milis += minutes.parse::<u64>()? * 60 * 1000;
                    milis += seconds.parse::<u64>()? * 1000;
                    milis += hundreths.parse::<u64>()? * 10;

                    milis = match offset {
                        Some(offset) if offset > 0 => milis.saturating_sub(offset.unsigned_abs()),
                        Some(offset) if offset < 0 => milis.saturating_add(offset.unsigned_abs()),
                        _ => milis,
                    };

                    result.lines.push(LrcLine {
                        time: Duration::from_millis(milis),
                        content: line.to_owned(),
                    });
                }
                Some(_) => {
                    let (key, value) = meta_or_time
                        .split_once(':')
                        .with_context(|| format!("Invalid metadata line: '{meta_or_time}'"))?;
                    match key.trim() {
                        "offset" => offset = Some(value.trim().parse()?),
                        "ti" => result.title = Some(value.trim().to_owned()),
                        "ar" => result.artist = Some(value.trim().to_owned()),
                        "al" => result.album = Some(value.trim().to_owned()),
                        "au" => result.author = Some(value.trim().to_owned()),
                        "length" => result.length = Some(parse_length(value.trim())?),
                        _ => {}
                    }
                }
                None => {
                    bail!("Invalid lrc metadata/timestamp: '{meta_or_time}'");
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use crate::shared::lrc::{lyrics::LrcLine, Lrc};

    #[test]
    fn lrc() {
        let input = r"[ti: asdf ]
[ar:123]
[al:333]
[au:444]
[length: 2:23]
[offset: +0]

[00:01.86]line with dot before hundredths
[00:04.73]line with colon before hundredths
[00:11.24]
[11:16.91]line with long time";

        let result: Lrc = input.parse().unwrap();

        assert_eq!(
            result,
            Lrc {
                title: Some("asdf".to_string()),
                artist: Some("123".to_string()),
                album: Some("333".to_string()),
                author: Some("444".to_string()),
                length: Some(Duration::from_secs(143)),
                lines: vec![
                    LrcLine {
                        time: Duration::from_millis(1860),
                        content: "line with dot before hundredths".to_string()
                    },
                    LrcLine {
                        time: Duration::from_millis(4730),
                        content: "line with colon before hundredths".to_string()
                    },
                    LrcLine {
                        time: Duration::from_millis(11240),
                        content: String::new()
                    },
                    LrcLine {
                        time: Duration::from_millis(676_910),
                        content: "line with long time".to_string()
                    },
                ],
            }
        );
    }

    #[test]
    fn lrc_offset_earlier() {
        let input = r"
[offset: +1000]

[00:01.86]line1
[00:04.73]line2
";

        let result: Lrc = input.parse().unwrap();

        assert_eq!(
            result,
            Lrc {
                title: None,
                artist: None,
                album: None,
                author: None,
                length: None,
                lines: vec![
                    LrcLine {
                        time: Duration::from_millis(860),
                        content: "line1".to_string()
                    },
                    LrcLine {
                        time: Duration::from_millis(3730),
                        content: "line2".to_string()
                    },
                ],
            }
        );
    }

    #[test]
    fn lrc_offset_later() {
        let input = r"
[offset: -1000]

[00:01.86]line1
[00:04.73]line2
";

        let result: Lrc = input.parse().unwrap();

        assert_eq!(
            result,
            Lrc {
                title: None,
                artist: None,
                album: None,
                author: None,
                length: None,
                lines: vec![
                    LrcLine {
                        time: Duration::from_millis(2860),
                        content: "line1".to_string()
                    },
                    LrcLine {
                        time: Duration::from_millis(5730),
                        content: "line2".to_string()
                    },
                ],
            }
        );
    }
}