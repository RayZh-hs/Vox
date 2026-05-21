#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OptimizationLevel {
    NOpt,
    #[default]
    IOpt,
    SOpt,
}
