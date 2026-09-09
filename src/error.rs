use std::{error::Error, fmt};

#[derive(Debug)]
pub(crate) enum LaunchError {
    UnknownProfile,
    UnknownEndpoint,
    DisabledProfile,
    IncompatibleEndpoint,
    MissingCredential(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProfile => formatter.write_str("Unknown profile"),
            Self::UnknownEndpoint => formatter.write_str("Unknown endpoint"),
            Self::DisabledProfile => formatter.write_str("Profile is disabled"),
            Self::IncompatibleEndpoint => {
                formatter.write_str("Endpoint protocol is incompatible with this agent")
            }
            Self::MissingCredential(name) => write!(
                formatter,
                "Required credential variable {name} is unset or empty"
            ),
        }
    }
}
impl Error for LaunchError {}
