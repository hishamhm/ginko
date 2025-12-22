use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub enum IncludeLoaderNotifyAction {
    None,
    Reset,
}

pub trait IncludeLoader {
    fn load(&mut self, relative_to: &Path, name: &str) -> Result<PathBuf, io::Error>;

    fn notify(&mut self, _path: &Path) -> IncludeLoaderNotifyAction {
        IncludeLoaderNotifyAction::None
    }

    fn set_include_paths(&mut self, include_paths: Vec<String>);

    fn watch_patterns(&self) -> Option<Vec<String>> {
        None
    }
}

#[derive(Clone)]
pub struct IncludeLoaderGuard {
    inner: Arc<Mutex<dyn IncludeLoader + Send>>,
}

impl IncludeLoaderGuard {
    pub fn new<L>(loader: L) -> Self
    where
        L: IncludeLoader + Send + 'static,
    {
        Self {
            inner: Arc::new(Mutex::new(loader)),
        }
    }

    pub fn load(&mut self, relative_to: &Path, file_name: &str) -> Result<PathBuf, io::Error> {
        let mut loader = self.inner.lock().expect("could not lock guard");
        loader.load(relative_to, file_name)
    }

    pub fn notify(&mut self, path: &Path) -> IncludeLoaderNotifyAction {
        let mut loader = self.inner.lock().expect("could not lock guard");
        loader.notify(path)
    }

    pub fn set_include_paths(&mut self, include_paths: Vec<String>) {
        let mut loader = self.inner.lock().expect("could not lock guard");
        loader.set_include_paths(include_paths);
    }

    pub fn watch_patterns(&self) -> Option<Vec<String>> {
        let loader = self.inner.lock().expect("could not lock guard");
        loader.watch_patterns()
    }
}

impl Default for IncludeLoaderGuard {
    fn default() -> Self {
        Self::new(DefaultIncludeLoader::default())
    }
}

#[derive(Default)]
pub struct DefaultIncludeLoader {
    include_paths: Vec<PathBuf>,
}

impl IncludeLoader for DefaultIncludeLoader {
    fn load(&mut self, relative_to: &Path, file_name: &str) -> Result<PathBuf, io::Error> {
        let include_resolved = self.include_paths.iter().find_map(|include_path| {
            let path = include_path.join(file_name);
            dunce::canonicalize(path).ok()
        });
        if let Some(include_resolved) = include_resolved {
            Ok(include_resolved)
        } else {
            dunce::canonicalize(
                relative_to
                    .parent()
                    .map(|p| p.join(file_name))
                    .unwrap_or_else(|| PathBuf::from(file_name)),
            )
        }
    }

    fn set_include_paths(&mut self, include_paths: Vec<String>) {
        self.include_paths = include_paths.iter().map(PathBuf::from).collect();
    }
}
