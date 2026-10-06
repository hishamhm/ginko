use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::dts::project::{ExternalAnalysis, ExternalAnalysisContext};
use crate::dts::tokens::IncludeKind;

pub enum IncludeLoaderNotifyAction {
    None,
    Reset,
}

pub trait IncludeLoader {
    fn load(
        &mut self,
        relative_to: &Path,
        target: &str,
        kind: IncludeKind,
    ) -> Result<PathBuf, io::Error>;

    fn notify(&mut self, _path: &Path) -> IncludeLoaderNotifyAction {
        IncludeLoaderNotifyAction::None
    }

    fn external_analysis(
        &mut self,
        _eactx: &ExternalAnalysisContext<'_>,
    ) -> Option<ExternalAnalysis> {
        None
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

    pub fn load(
        &mut self,
        relative_to: &Path,
        target: &str,
        kind: IncludeKind,
    ) -> Result<PathBuf, io::Error> {
        let mut loader = self.inner.lock().expect("could not lock guard");
        loader.load(relative_to, target, kind)
    }

    pub fn notify(&mut self, path: &Path) -> IncludeLoaderNotifyAction {
        let mut loader = self.inner.lock().expect("could not lock guard");
        loader.notify(path)
    }

    pub fn external_analysis(
        &self,
        eactx: &ExternalAnalysisContext<'_>,
    ) -> Option<ExternalAnalysis> {
        let mut loader = self.inner.lock().expect("could not lock guard");

        loader.external_analysis(eactx)
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
    fn load(
        &mut self,
        relative_to: &Path,
        target: &str,
        kind: IncludeKind,
    ) -> Result<PathBuf, io::Error> {
        let in_include_paths = || {
            self.include_paths.iter().find_map(|include_path| {
                let full_path = include_path.join(target);
                if let Ok(true) = std::fs::exists(&full_path) {
                    Some(full_path)
                } else {
                    None
                }
            })
        };

        let relative = || {
            let full = relative_to
                .parent()
                .map(|p| p.join(target))
                .unwrap_or_else(|| PathBuf::from(target));
            std::fs::exists(&full).is_ok_and(|v| v).then_some(full)
        };

        let resolved = match kind {
            IncludeKind::Dts | IncludeKind::CQuoted => relative().or_else(in_include_paths),
            IncludeKind::CAngled => in_include_paths(),
        };

        resolved.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("file not found: {target}"))
        })
    }

    fn set_include_paths(&mut self, include_paths: Vec<String>) {
        self.include_paths = include_paths.iter().map(PathBuf::from).collect();
    }
}

#[cfg(test)]
mod tests {
    use crate::dts::{DefaultIncludeLoader, IncludeLoader};

    #[test]
    fn loading_priority() {
        let root = tempfile::tempdir().unwrap();
        let global_header = root.path().join("header.h");
        std::fs::write(&global_header, b"a").unwrap();

        let project = tempfile::tempdir().unwrap();
        let local_header = project.path().join("header.h");
        std::fs::write(&local_header, b"a").unwrap();

        let project_file = project.path().join("device.dts");
        std::fs::write(&project_file, b"a").unwrap();

        let project_local_header = project.path().join("lheader.h");
        std::fs::write(project_local_header, b"a").unwrap();

        let mut loader = DefaultIncludeLoader::default();

        loader.set_include_paths(vec![root.path().to_str().unwrap().to_string()]);

        assert_eq!(
            loader
                .load(
                    &project_file,
                    "header.h",
                    crate::dts::tokens::IncludeKind::Dts
                )
                .unwrap(),
            local_header
        );

        assert_eq!(
            loader
                .load(
                    &project_file,
                    "header.h",
                    crate::dts::tokens::IncludeKind::CQuoted
                )
                .unwrap(),
            local_header
        );

        assert_eq!(
            loader
                .load(
                    &project_file,
                    "header.h",
                    crate::dts::tokens::IncludeKind::CAngled
                )
                .unwrap(),
            global_header
        );

        assert!(loader
            .load(
                &project_file,
                "lheader.h",
                crate::dts::tokens::IncludeKind::CAngled
            )
            .is_err(),)
    }
}
