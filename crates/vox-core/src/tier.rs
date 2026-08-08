#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum LanguageTier {
    Inline = 0,
    Eval = 1,
    #[default]
    Script = 2,
    Dev = 3,
    Debug = 4,
}

impl LanguageTier {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Eval => "eval",
            Self::Script => "script",
            Self::Dev => "dev",
            Self::Debug => "debug",
        }
    }

    pub const fn supports(self, required: Self) -> bool {
        self.as_u8() >= required.as_u8()
    }
}

impl TryFrom<u8> for LanguageTier {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Inline),
            1 => Ok(Self::Eval),
            2 => Ok(Self::Script),
            3 => Ok(Self::Dev),
            4 => Ok(Self::Debug),
            _ => Err("language tier must be between 0 and 4"),
        }
    }
}
