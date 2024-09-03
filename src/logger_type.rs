pub type Logger = std::sync::Arc<dyn Fn(&str) + Send + Sync + 'static>;
