use crate::dts::analysis::{Analysis, AnalysisContext};
use crate::dts::ast::{DtsFile, Include, Reference};
use crate::dts::data::HasSource;
use crate::dts::error_codes::SeverityMap;
use crate::dts::loader::IncludeLoaderGuard;
use crate::dts::reader::ByteReader;
use crate::dts::tokens::Lexer;
use crate::dts::visitor::{ItemAtCursor, ReferenceContext};
use crate::dts::{Diagnostic, FileType, HasSpan, Parser, ParserConfig, Position, Severity, Span};
use std::collections::{BTreeSet, HashMap};
use std::iter::empty;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

pub enum AnalysisStatus {
    NotAnalyzed,
    Root(AnalysisContext),
    Child,
}

pub struct ProjectFile {
    pub(crate) parent: Option<PathBuf>,
    pub(crate) includes: Vec<PathBuf>,
    pub(crate) parser_diagnostics: Vec<Diagnostic>,
    pub(crate) analysis_diagnostics: Vec<Diagnostic>,
    pub(crate) file: Option<DtsFile>,
    pub(crate) file_type: FileType,
    pub(crate) source: String,
    pub(crate) analysis_status: AnalysisStatus,
}

impl ProjectFile {
    pub fn parsed(
        parent: Option<PathBuf>,
        includes: Vec<PathBuf>,
        diagnostics: Vec<Diagnostic>,
        file_type: FileType,
        file: DtsFile,
        source: String,
    ) -> ProjectFile {
        ProjectFile {
            parent,
            includes,
            parser_diagnostics: diagnostics,
            file: Some(file),
            file_type,
            source,
            analysis_diagnostics: vec![],
            analysis_status: AnalysisStatus::NotAnalyzed,
        }
    }

    pub fn sentinel() -> ProjectFile {
        ProjectFile {
            parent: None,
            includes: vec![],
            parser_diagnostics: vec![],
            analysis_diagnostics: vec![],
            file: None,
            source: "".to_string(),
            file_type: FileType::Unknown,
            analysis_status: AnalysisStatus::NotAnalyzed,
        }
    }

    pub fn unrecoverable(err: Diagnostic, source: String, file_type: FileType) -> ProjectFile {
        ProjectFile {
            parent: None,
            includes: vec![],
            parser_diagnostics: vec![err],
            analysis_diagnostics: vec![],
            file: None,
            source,
            file_type,
            analysis_status: AnalysisStatus::NotAnalyzed,
        }
    }

    pub fn diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.parser_diagnostics
            .iter()
            .chain(&self.analysis_diagnostics)
    }

    pub fn clear_diagnostics(&mut self) {
        self.parser_diagnostics.clear();
        self.analysis_diagnostics.clear();
    }

    pub fn has_errors(&self, severity_map: &SeverityMap) -> bool {
        self.diagnostics()
            .any(|diagnostic| diagnostic.severity(severity_map) == Severity::Error)
    }

    pub fn source(&self) -> &String {
        &self.source
    }
}

#[derive(Default)]
pub struct Project {
    files: HashMap<PathBuf, ProjectFile>,
    root_file: Option<PathBuf>,
    pub severities: SeverityMap,
    config: ParserConfig,
    canonicalizer: Option<fn(&Path) -> io::Result<PathBuf>>,
}

impl Project {
    pub fn set_parser_config(&mut self, config: ParserConfig) {
        self.config = config;
    }

    pub fn set_canonicalizer(&mut self, f: fn(&Path) -> io::Result<PathBuf>) {
        self.canonicalizer = Some(f);
    }

    fn canonicalize<P: AsRef<Path>>(&self, path: P) -> io::Result<PathBuf> {
        if let Some(f) = self.canonicalizer {
            f(path.as_ref())
        } else {
            self::fs::canonicalize(path)
        }
    }

    pub fn set_root_file_and_reset(
        &mut self,
        file_name: String,
        loader: &mut IncludeLoaderGuard,
    ) -> Result<(), io::Error> {
        let file_name = self.canonicalize(file_name)?;
        self.root_file = Some(file_name);
        self.reset(loader)
    }

    pub fn reset(&mut self, loader: &mut IncludeLoaderGuard) -> Result<(), io::Error> {
        let Some(root_file) = self.root_file.clone() else {
            return Ok(());
        };
        self.remove_tree(root_file.as_path());
        self.add_path_buf(root_file.clone(), loader)?;
        Ok(())
    }

    fn remove_tree(&mut self, root_file: &Path) {
        let mut files_to_remove = BTreeSet::<_>::new();

        // Tree traversal stack
        let mut stack = vec![root_file];

        while let Some(file) = stack.pop() {
            files_to_remove.insert(file.to_path_buf());
            if let Some(project_file) = self.files.get(file) {
                for include in &project_file.includes {
                    stack.push(include);
                }
            }
        }

        for file in files_to_remove {
            self.files.remove(&file);
        }
    }

    pub fn add_file(
        &mut self,
        file_name: String,
        loader: &mut IncludeLoaderGuard,
    ) -> Result<(), io::Error> {
        let file_name = self.canonicalize(file_name)?;
        self.add_path_buf(file_name, loader)
    }

    fn add_path_buf(
        &mut self,
        file_name: PathBuf,
        loader: &mut IncludeLoaderGuard,
    ) -> Result<(), io::Error> {
        let content = fs::read_to_string(&file_name)?;
        let file_ending = FileType::from(file_name.as_path());
        self.add_file_with_text(file_name, content, file_ending, loader);
        Ok(())
    }

    /// Adds a file to the project with already given text.
    /// Re-evaluates the file, if the file is already present.
    /// Does not re-evaluate dependencies, if they are cached.
    ///
    /// # Parameters
    /// * file_name: The name of the file.
    /// * text: The contents of the file.
    /// * file_type: Defines how the file should be analyzed.
    /// * loader: The loader engine for resolving `/include/` paths.
    ///
    /// # Panics
    /// If `file_name` does not point to a valid file.
    pub fn add_file_with_text(
        &mut self,
        file_name: PathBuf,
        text: String,
        file_type: FileType,
        loader: &mut IncludeLoaderGuard,
    ) {
        let file_name = self.canonicalize(file_name).expect("File must be present");

        // First step: Parse file and all dependencies.
        // Dependencies are cached.
        //
        // Here, `parent` is set to None. That means that, for the root file,
        // the resulting ProjectFile no `parent`; for any "child" files, they will
        // reattach to their previous position in the tree, or, in the case of
        // a new full scan from the root, their will be assigned their correct
        // `parent` values.
        self.parse_file(&file_name, text, file_type, None, loader);
        self.analyze_tree_for(&file_name, loader);
    }

    fn recursive_analysis(
        &mut self,
        analysis: &mut Analysis,
        file: &Path,
        parent: Option<&Path>,
        seen: &mut BTreeSet<PathBuf>,
    ) -> Option<()> {
        if seen.contains(file) {
            return None;
        }
        seen.insert(file.to_path_buf());

        for include in self.files.get(file)?.includes.clone() {
            self.recursive_analysis(analysis, &include, Some(file), seen);
        }

        let proj_file = self.files.get(file)?;
        if let Some(dts_file) = &proj_file.file {
            let result = analysis.analyze_file(dts_file, proj_file.file_type, self);
            let proj_file = self.files.get_mut(file)?;
            proj_file.analysis_status = AnalysisStatus::Child;
            proj_file.parent = parent.map(Path::to_path_buf);
            proj_file.analysis_diagnostics = result.diagnostics;
        }
        Some(())
    }

    /// Find the analysis root for a given file. It will either return the file itself,
    /// or the root of the include-tree it is a part of.
    fn analysis_root_for(&self, file: &Path) -> Option<(PathBuf, &AnalysisStatus)> {
        Some({
            let mut current = file;
            loop {
                let proj_file = self.files.get(current)?;
                if let Some(parent) = &proj_file.parent {
                    current = parent;
                } else {
                    break (current.to_path_buf(), &proj_file.analysis_status);
                }
            }
        })
    }

    pub fn analyze_tree_for(&mut self, element: &Path, loader: &mut IncludeLoaderGuard) {
        let (root, include_cache) = {
            let Some((root, root_status)) = self.analysis_root_for(element) else {
                return;
            };
            let include_cache = match root_status {
                AnalysisStatus::Root(context) => context.get_includes().clone(),
                _ => Default::default(),
            };
            (root, include_cache)
        };

        let mut analysis = Analysis::new(Some(include_cache), loader);
        let mut seen = BTreeSet::new();

        self.recursive_analysis(&mut analysis, &root, None, &mut seen);

        let Some(proj_file) = self.files.get_mut(&root) else {
            return;
        };
        proj_file.analysis_status = AnalysisStatus::Root(analysis.into_context())
    }

    pub fn get_diagnostics(&self, path: &Path) -> Box<dyn Iterator<Item = &Diagnostic> + '_> {
        let Some(file) = self.get_file(path) else {
            return Box::new(empty());
        };
        Box::new(file.diagnostics())
    }

    pub fn all_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.files.values().flat_map(|file| file.diagnostics())
    }

    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(|key| key.as_path())
    }

    pub fn project_files(&self) -> impl Iterator<Item = &ProjectFile> {
        self.files.values()
    }

    pub fn get_analysis(&self, path: &Path) -> Option<&AnalysisContext> {
        let (_, analysis_status) = self.analysis_root_for(path)?;
        match analysis_status {
            AnalysisStatus::Root(context) => Some(context),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn assert_no_diagnostics(&self) {
        use itertools::Itertools;
        let diagnostics = self
            .files
            .values()
            .flat_map(|file| file.diagnostics())
            .collect_vec();
        if diagnostics.is_empty() {
            return;
        }
        for diag in diagnostics {
            println!("{diag:?}");
        }
        panic!("Found diagnostics")
    }

    pub fn find_at_pos<'a>(&'a self, path: &Path, position: &Position) -> Option<ItemAtCursor<'a>> {
        let file = self.get_file(path).and_then(|file| file.file.as_ref())?;
        file.item_at_cursor(position)
    }

    pub fn document_reference(
        &self,
        path: &Path,
        reference: &Reference,
        ctx: &ReferenceContext<'_>,
    ) -> Option<String> {
        let name = self.get_analysis(path)?.get_referred(reference, ctx)?;
        Some(name)
    }

    pub fn get_node_position(
        &self,
        path: &Path,
        reference: &Reference,
        ctx: &ReferenceContext<'_>,
    ) -> Option<(Span, Arc<Path>)> {
        self.get_analysis(path)?.get_position(reference, ctx)
    }

    pub fn get_root(&self, path: &Path) -> Option<&DtsFile> {
        match self.get_file(path) {
            Some(ProjectFile {
                file: Some(file), ..
            }) => Some(file),
            _ => None,
        }
    }

    fn parse_file(
        &mut self,
        file_name: &Path,
        text: String,
        file_type: FileType,
        parent: Option<PathBuf>,
        loader: &mut IncludeLoaderGuard,
    ) {
        let reader = ByteReader::from_string(text.clone());
        let lexer = Lexer::new(reader, file_name.into());

        let parent = if parent.is_some() {
            parent
        } else if let Some(old) = self.files.get(file_name) {
            old.parent.clone()
        } else {
            None
        };

        let mut parser = Parser::new(lexer, self.config.clone());

        match parser.file() {
            Ok(file) => {
                // insert sentinel so that no cyclic dependency can occur.
                self.files
                    .insert(file_name.to_path_buf(), ProjectFile::sentinel());

                // Recurse into includes.
                let includes: Vec<PathBuf> = file
                    .elements
                    .iter()
                    .filter_map(|primary| primary.as_include())
                    .filter_map(|include| {
                        self.parse_included_file(
                            &mut parser.diagnostics,
                            &file.source,
                            include,
                            loader,
                        )
                    })
                    .collect();

                // Insert the file proper, with the `parent` and `includes` forming a tree.
                self.files.insert(
                    file_name.to_path_buf(),
                    ProjectFile::parsed(
                        parent,
                        includes,
                        parser.diagnostics,
                        file_type,
                        file,
                        text,
                    ),
                );
            }
            Err(err) => {
                self.files.insert(
                    file_name.to_path_buf(),
                    ProjectFile::unrecoverable(err, text, file_type),
                );
            }
        };
    }

    fn parse_included_file(
        &mut self,
        diagnostics: &mut Vec<Diagnostic>,
        parent: &Path,
        include: &Include,
        loader: &mut IncludeLoaderGuard,
    ) -> Option<PathBuf> {
        let canonicalized_path = self
            .canonicalize(loader.load(parent, &include.file_name()).ok()?)
            .ok()?;

        // Avoids duplicate insertion and cyclic dependencies
        if !self.files.contains_key(&canonicalized_path) {
            match fs::read_to_string(&canonicalized_path) {
                Ok(text) => {
                    let typ = FileType::from(canonicalized_path.as_path());
                    self.parse_file(
                        &canonicalized_path,
                        text,
                        typ,
                        Some(parent.to_path_buf()),
                        loader,
                    );
                }
                Err(err) => {
                    diagnostics.push(Diagnostic::io_error(include.span(), include.source(), err))
                }
            }
        }
        Some(canonicalized_path)
    }

    pub fn get_file(&self, path: &Path) -> Option<&ProjectFile> {
        match self.canonicalize(path) {
            Ok(path) => self.files.get(&path),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
// For some reason, this fails under windows with error "The system cannot find the file specified. (os error 2)"
#[cfg(not(windows))]
mod tests {
    use crate::dts::ast::PropertyPath;
    use crate::dts::error_codes::ErrorCode;
    use crate::dts::loader::IncludeLoaderGuard;
    use crate::dts::test::Code;
    use crate::dts::tokens::TokenKind;
    use crate::dts::visitor::ReferenceContext;
    use crate::dts::{ast, Diagnostic, HasSpan, ItemAtCursor, Project};
    use assert_matches::assert_matches;
    use itertools::Itertools;
    use std::fs;
    use std::fs::File;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    struct TempDir {
        pub inner: tempfile::TempDir,
    }

    impl TempDir {
        pub fn new() -> TempDir {
            let dir = tempdir().expect("Cannot create temporary directory");
            TempDir { inner: dir }
        }

        pub fn add_file(
            &self,
            name: impl AsRef<Path>,
            content: impl AsRef<str>,
        ) -> (Code, PathBuf) {
            let code = Code::new(content.as_ref());
            let file_path = self.inner.path().join(name);

            fs::write(&file_path, code.code()).expect("Cannot write to file");
            (code, file_path)
        }

        pub fn new_file(&self, name: impl AsRef<Path>) -> (File, PathBuf) {
            let file_path = self.inner.path().join(name);
            (
                File::create(&file_path).expect("Cannot create temporary file"),
                file_path,
            )
        }
    }

    #[test]
    pub fn file_with_includes() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();
        let temp_dir = TempDir::new();
        let (_, path1) = temp_dir.add_file("tests-include.dtsi", "");
        project
            .add_file(
                path1.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");

        let (_, file2) = temp_dir.add_file(
            "tests-file.dts",
            format!(
                r#"
/dts-v1/;

/include/ "{}"
"#,
                path1.display()
            ),
        );
        project
            .add_file(
                file2.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");
        assert!(project.get_file(&path1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert_eq!(project.get_diagnostics(&path1).next(), None);
        assert_eq!(project.get_diagnostics(&file2).next(), None);
        assert!(project.get_root(&path1).is_some());
        assert!(project.get_analysis(&path1).is_some());
        assert!(project.get_analysis(&file2).is_some());
    }

    #[test]
    pub fn cross_file_references() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();
        let temp_dir = TempDir::new();
        let (code1, file1) = temp_dir.add_file(
            "tests-include.dtsi",
            r#"
/ {
    some_node: node_a {
        // ...
    };
};
"#,
        );
        let (code2, file2) = temp_dir.add_file(
            "tests-file.dts",
            format!(
                r#"
/dts-v1/;

/include/ "{}"

&some_node {{
}};

&{{/node_a}} {{
}};
"#,
                file1.display()
            ),
        );

        project
            .add_file(
                file2.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project.assert_no_diagnostics();

        let substr = code2.s1("&some_node");
        let item = project
            .find_at_pos(&file2, &substr.span().start())
            .expect("Found no item");
        let ItemAtCursor::Reference(reference, _) = item else {
            panic!("Found non-node at cursor")
        };
        assert_eq!(reference, &ast::Reference::Label("some_node".to_owned()));
        match project.get_node_position(&file2, reference, &ReferenceContext::Root) {
            Some((span, path)) => {
                assert_eq!(span, code1.s1("node_a").span());
                assert_eq!(
                    path.to_path_buf(),
                    project.canonicalize(&file1).expect("File does not exist")
                );
            }
            None => panic!("References does not reference nodes"),
        }

        let substr = code2.s1("&{/node_a}");
        let item = project
            .find_at_pos(&file2, &substr.span().start())
            .expect("Found no item");
        let ItemAtCursor::Reference(reference, _) = item else {
            panic!("Found non-node at cursor")
        };
        assert_eq!(reference, &ast::Reference::Path("/node_a".into()));
        match project.get_node_position(&file2, reference, &ReferenceContext::Root) {
            Some((span, path)) => {
                assert_eq!(span, code1.s1("node_a").span());
                assert_eq!(
                    path.to_path_buf(),
                    project.canonicalize(&file1).expect("File does not exist")
                );
            }
            None => panic!("References does not reference nodes"),
        }
    }

    #[test]
    pub fn cyclic_import_error() {
        let temp_dir = TempDir::new();
        let (mut file1, path1) = temp_dir.new_file("test.dts");
        let (mut file2, path2) = temp_dir.new_file("tests-include.dtsi");

        write!(file1, r#"/dts-v1/; /include/ "{}""#, path2.display())
            .expect("Cannot write to file 2");
        write!(file2, r#"/include/ "{}""#, path1.display()).expect("Cannot write to file1");

        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();
        project
            .add_file(path1.into_os_string().into_string().unwrap(), &mut loader)
            .expect("Cannot add file to project");

        let diag = project.all_diagnostics().cloned().collect_vec();
        // This is explicitly vague. There should be some error somewhere,
        // but the exact location is not perfect at the moment.
        // This is because the error only occurs in one file while it should occur in all
        // files affected by the cyclic include.
        assert_matches!(
            &diag[..],
            &[Diagnostic {
                kind: ErrorCode::CyclicDependencyError,
                ..
            }]
        );
    }

    #[test]
    pub fn file_with_multiple_includes() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let temp_dir = TempDir::new();
        let (_, file1) = temp_dir.add_file("test1.dtsi", "");
        project
            .add_file(
                file1.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");
        let (_, file2) = temp_dir.add_file("test2.dtsi", "");
        project
            .add_file(
                file2.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");
        let (_, file3) = temp_dir.add_file(
            "test.dts",
            format!(
                r#"
/dts-v1/;

/include/ "{}"
/include/ "{}"
"#,
                file1.display(),
                file2.display()
            ),
        );

        project
            .add_file(file3.into_os_string().into_string().unwrap(), &mut loader)
            .expect("Unexpected IO error");
        project.assert_no_diagnostics();
    }

    #[test]
    pub fn file_with_nested_includes() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let temp_dir = TempDir::new();
        let (_, file1) = temp_dir.add_file("tests-include1.dtsi", "");
        let (_, file2) = temp_dir.add_file(
            "tests-include2.dtsi",
            format!(r#"/include/ "{}""#, file1.display()).as_str(),
        );
        let (_, file3) = temp_dir.add_file(
            "test.dts",
            format!(
                r#"
/dts-v1/;

/include/ "{}"
"#,
                file2.display()
            ),
        );

        project
            .add_file(
                file3.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project.assert_no_diagnostics();

        assert!(project.get_file(&file1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert!(project.get_file(&file3).is_some());
    }

    // TODO removal by trees is yet to be implemented
    /*
    #[test]
    pub fn remove_file_uses_refcount() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let temp_dir = TempDir::new();
        let (_, file1) = temp_dir.add_file("tests-include1.dtsi", "");
        let (_, file2) = temp_dir.add_file(
            "tests-include2.dtsi",
            format!(r#"/include/ "{}""#, file1.display()).as_str(),
        );
        let (_, file3) = temp_dir.add_file(
            "test3.dts",
            format!(
                r#"
    /dts-v1/;

    /include/ "{}"
    "#,
                file2.display()
            ),
        );
        let (_, file4) = temp_dir.add_file(
            "test4.dts",
            format!(
                r#"
    /dts-v1/;

    /include/ "{}"
    "#,
                file2.display()
            ),
        );

        project
            .add_file(
                file3.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project
            .add_file(
                file4.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project.assert_no_diagnostics();

        assert!(project.get_file(&file1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert!(project.get_file(&file3).is_some());
        assert!(project.get_file(&file4).is_some());

        // Equivalent to opening the included files by themselves in an editor:
        // this increases their reference count.
        project
            .add_file(
                file1.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");
        project
            .add_file(
                file2.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file");

        assert!(project.get_file(&file1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert!(project.get_file(&file3).is_some());
        assert!(project.get_file(&file4).is_some());
    }
    */

    #[test]
    pub fn bad_path_in_include_and_incbin() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let temp_dir = TempDir::new();
        let (code, file) = temp_dir.add_file(
            "test.dts",
            r#"
/dts-v1/;

/include/ "/missing_file_1"

/ {
    prop = /incbin/( "/missing_file_2" );
};
"#,
        );
        project
            .add_file(
                file.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file to project");

        assert!(project.get_file(&file).is_some());
        let diagnostics = project.get_diagnostics(&file).cloned().collect_vec();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(*diagnostics[0].kind(), ErrorCode::IOError);
        assert_eq!(
            diagnostics[0].span(),
            code.s1("/include/ \"/missing_file_1\"").span()
        );
        assert_eq!(*diagnostics[1].kind(), ErrorCode::IOError);
        assert_eq!(
            diagnostics[1].span(),
            code.s1("/incbin/( \"/missing_file_2\" )").span()
        );
    }

    #[test]
    pub fn error_in_included_file_add_include_before_dts() {
        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let temp_dir = TempDir::new();
        let (code1, file1) = temp_dir.add_file("error.dtsi", "/ {}"); // missing semicolon
        let (code2, file2) = temp_dir.add_file(
            "test.dts",
            format!(
                r#"
/dts-v1/;

/include/ "{}"
"#,
                file1.display()
            ),
        );
        project
            .add_file(
                file2.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Cannot add file to project");

        assert!(project.get_file(&file1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert_eq!(
            project.get_diagnostics(&file1).cloned().collect_vec(),
            vec![Diagnostic::expected(
                code1.s1("}").end().as_span(),
                project
                    .canonicalize(&file1)
                    .expect("Cannot canonicalize")
                    .into(),
                &[TokenKind::Semicolon]
            )]
        );
        assert_eq!(
            project.get_diagnostics(&file2).cloned().collect_vec(),
            vec![Diagnostic::new(
                code2
                    .s1(format!(r#"/include/ "{}""#, file1.display()).as_str())
                    .span(),
                project
                    .canonicalize(file2)
                    .expect("Cannot canonicalize")
                    .into(),
                ErrorCode::ErrorsInInclude,
                "Included file contains errors"
            )]
        );
    }

    #[test]
    pub fn file_with_include_paths_includes() {
        let includes_dir = TempDir::new();
        let (_, file1) = includes_dir.add_file("tests-include1.dtsi", "");
        let another_includes_dir = TempDir::new();
        let (_, file2) = another_includes_dir.add_file("tests-include2.dtsi", "");

        let temp_dir = TempDir::new();
        let (_, file3) = temp_dir.add_file(
            "test.dts",
            r#"
/dts-v1/;

/include/ "tests-include1.dtsi"
/include/ "tests-include2.dtsi"
"#,
        );

        let mut project = Project::default();
        let mut loader = IncludeLoaderGuard::default();

        let include_paths = vec![
            includes_dir.inner.path().display().to_string(),
            another_includes_dir.inner.path().display().to_string(),
        ];

        loader.set_include_paths(include_paths);

        project
            .add_file(
                file3.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project.assert_no_diagnostics();

        assert!(project.get_file(&file1).is_some());
        assert!(project.get_file(&file2).is_some());
        assert!(project.get_file(&file3).is_some());
    }

    #[test]
    pub fn file_with_dot_relative_references() {
        let temp_dir = TempDir::new();
        let (code, file) = temp_dir.add_file(
            "test.dts",
            r#"
/dts-v1/;

/ {
    some_label: node {
        relative = ${./another/nested/prop};
        second_label: another {
            nested {
                prop = <0>;
            };
        };
    };
};
"#,
        );

        let mut project = Project::default();
        eprintln!("{}", file.display());
        let mut loader = IncludeLoaderGuard::default();

        project
            .add_file(
                file.clone().into_os_string().into_string().unwrap(),
                &mut loader,
            )
            .expect("Unexpected IO error");

        project.assert_no_diagnostics();

        assert!(project.get_file(&file).is_some());

        let item = project
            .find_at_pos(&file, &code.s1("nested").position())
            .unwrap();

        let ItemAtCursor::Reference(reference, ref_ctx) = item else {
            panic!("Found non-node at cursor")
        };

        let ReferenceContext::Node(_) = &ref_ctx else {
            panic!("Reference has no context")
        };

        assert_eq!(
            reference,
            &ast::Reference::PropertyPath(PropertyPath::new(
                ast::Path::new_dot_relative(vec!["another".into(), "nested".into()]),
                "prop".into()
            ))
        );
        match project.get_node_position(&file, reference, &ref_ctx) {
            Some((span, path)) => {
                assert_eq!(span, code.s("prop", 2).span());
                assert_eq!(
                    path.to_path_buf(),
                    project.canonicalize(&file).expect("File does not exist")
                );
            }
            None => panic!("References does not reference nodes"),
        }
    }
}
