use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    EnZh,
    ZhEn,
}

impl Direction {
    pub fn key(self) -> &'static str {
        match self {
            Self::EnZh => "en_zh",
            Self::ZhEn => "zh_en",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::EnZh => "EN → 中文",
            Self::ZhEn => "中文 → EN",
        }
    }

    pub fn prompt_version(self) -> &'static str {
        match self {
            Self::EnZh => "en-zh-v2",
            Self::ZhEn => "zh-en-v1",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectionChoice {
    #[default]
    Auto,
    EnZh,
    ZhEn,
}

impl DirectionChoice {
    pub const ALL: [Self; 3] = [Self::Auto, Self::EnZh, Self::ZhEn];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::EnZh => "EN → 中文",
            Self::ZhEn => "中文 → EN",
        }
    }

    pub fn resolve(self, input: &str) -> Result<Direction> {
        match self {
            Self::EnZh => return Ok(Direction::EnZh),
            Self::ZhEn => return Ok(Direction::ZhEn),
            Self::Auto => {}
        }
        let mut han = false;
        let mut latin = false;
        let mut other = false;
        for c in input.chars() {
            if is_han(c) {
                han = true;
            } else if c.is_alphabetic() {
                if matches!(c, 'A'..='Z' | 'a'..='z' | '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}' | '\u{ff21}'..='\u{ff3a}' | '\u{ff41}'..='\u{ff5a}')
                {
                    latin = true;
                } else {
                    other = true;
                }
            }
        }
        match (han, latin, other) {
            (true, false, false) => Ok(Direction::ZhEn),
            (false, true, false) => Ok(Direction::EnZh),
            _ => bail!("Choose EN → 中文 or 中文 → EN for mixed or ambiguous text."),
        }
    }
}

pub fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}' | '\u{30000}'..='\u{323af}')
}

/// Records written before direction selection were explicitly English → Chinese.
pub fn legacy_direction_choice() -> DirectionChoice {
    DirectionChoice::EnZh
}
