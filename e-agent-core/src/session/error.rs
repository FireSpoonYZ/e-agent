#[derive(Debug, thiserror::Error)]
pub enum Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;
