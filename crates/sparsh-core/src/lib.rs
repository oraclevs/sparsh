mod alias;
pub mod builtin;
mod dispatch;
mod environment;
mod execute;
mod path;
mod resolver;
mod services;
mod session;
pub mod value;

pub use builtin::{BuiltinError, BuiltinMetadata, BuiltinOutput, BuiltinRegistry};
pub use session::{ShellError, ShellResult, ShellSession};
pub use value::render_value;

pub const PRODUCT_NAME: &str = "sparsh";

#[cfg(test)]
pub(crate) static PROCESS_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    #[test]
    fn identifies_the_core_product() {
        assert_eq!(super::PRODUCT_NAME, "sparsh");
    }
}
