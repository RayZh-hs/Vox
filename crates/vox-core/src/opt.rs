#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OptimizationLevel {
    NOpt,
    #[default]
    IOpt,
    SOpt,
}

impl OptimizationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NOpt => "NOpt",
            Self::IOpt => "IOpt",
            Self::SOpt => "SOpt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OptimizationRank {
    Baseline,
    Interactive,
    SealedOwnership,
    SealedDemand,
    SealedMaterialization,
}

impl OptimizationRank {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Interactive => "interactive",
            Self::SealedOwnership => "sealed-ownership",
            Self::SealedDemand => "sealed-demand",
            Self::SealedMaterialization => "sealed-materialization",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationSubject {
    Module,
    Function(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationRanking {
    pub subject: OptimizationSubject,
    pub rank: OptimizationRank,
}
