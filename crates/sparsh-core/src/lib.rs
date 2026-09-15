pub mod builtin;
pub mod value;

pub use builtin::{BuiltinError, BuiltinMetadata, BuiltinOutput, BuiltinRegistry};
pub use value::render_value;

pub const PRODUCT_NAME: &str = "sparsh";

#[cfg(test)]
mod tests {
    #[test]
    fn identifies_the_core_product() {
        assert_eq!(super::PRODUCT_NAME, "sparsh");
    }
}
