#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptimizationLevel {
    NOpt,
    #[default]
    IOpt,
    SOpt,
}
