use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use failure::Error;
use futures::Future;
use futures::future::join_all;
use git_ls_remote::{LsRemote, LsRemoteRequest, ObjectId};
use hyper::Client;
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use relative_path::{RelativePath, RelativePathBuf};
use slog::Logger;
use tokio_service::Service;

mod machines;
mod futures;

use ::utils::cache::Cache;

use ::models::repo::{Repository, RepoPath};
use ::models::crates::{CrateName, CrateRelease, AnalyzedDependencies};

use ::interactors::crates::QueryCrate;
use ::interactors::RetrieveFileAtPath;
use ::interactors::github::{GetPopularRepos};

use self::futures::AnalyzeDependenciesFuture;
use self::futures::CrawlManifestFuture;

type HttpClient = Client<HttpsConnector<HttpConnector>>;

#[derive(Clone, Debug)]
pub struct Engine {
    client: HttpClient,
    logger: Logger,

    git_ls_remote: Arc<LsRemote<HttpsConnector<HttpConnector>>>,
    query_crate: Arc<Cache<QueryCrate<HttpClient>>>,
    get_popular_repos: Arc<Cache<GetPopularRepos<HttpClient>>>,
    retrieve_file_at_path: Arc<Cache<RetrieveFileAtPath<HttpClient>>>
}

impl Engine {
    pub fn new(client: Client<HttpsConnector<HttpConnector>>, logger: Logger) -> Engine {
        let git_ls_remote = LsRemote::new(client.clone());
        let query_crate = Cache::new(QueryCrate(client.clone()), Some(Duration::from_secs(300)), 500);
        let get_popular_repos = Cache::new(GetPopularRepos(client.clone()), Some(Duration::from_secs(10)), 1);
        let retrieve_file_at_path = Cache::new(RetrieveFileAtPath(client.clone()), None, 500);

        Engine {
            client: client.clone(), logger,

            git_ls_remote: Arc::new(git_ls_remote),
            query_crate: Arc::new(query_crate),
            get_popular_repos: Arc::new(get_popular_repos),
            retrieve_file_at_path: Arc::new(retrieve_file_at_path)
        }
    }
}

pub struct AnalyzeDependenciesOutcome {
    pub crates: Vec<(CrateName, AnalyzedDependencies)>,
    pub duration: Duration
}

impl AnalyzeDependenciesOutcome {
    pub fn any_outdated(&self) -> bool {
        self.crates.iter().any(|&(_, ref deps)| deps.any_outdated())
    }

    pub fn outdated_ratio(&self) -> (usize, usize) {
        self.crates.iter().fold((0, 0), |(outdated, total), &(_, ref deps)| {
            (outdated + deps.count_outdated(), total + deps.count_total())
        })
    }
}

impl Engine {
    pub fn get_popular_repos(&self) ->
        impl Future<Item=Vec<Repository>, Error=Error>
    {
        self.get_popular_repos.call(())
            .from_err().map(|repos| {
                repos.iter()
                    .filter(|repo| !POPULAR_REPOS_BLACKLIST.contains(&repo.path))
                    .cloned().collect()
            })
    }

    pub fn analyze_dependencies(&self, repo_path: RepoPath) ->
        impl Future<Item=AnalyzeDependenciesOutcome, Error=Error>
    {
        let start = Instant::now();
        let engine = self.clone();

        self.find_head_oid(&repo_path).and_then(move |oid| {
            let entry_point = RelativePath::new("/").to_relative_path_buf();
            let manifest_future = CrawlManifestFuture::new(&engine, repo_path, oid, entry_point);

            manifest_future.and_then(move |manifest_output| {
                let futures = manifest_output.crates.into_iter().map(move |(crate_name, deps)| {
                    let analyzed_deps_future = AnalyzeDependenciesFuture::new(&engine, deps);

                    analyzed_deps_future.map(move |analyzed_deps| (crate_name, analyzed_deps))
                });

                join_all(futures).map(move |crates| {
                    let duration = start.elapsed();

                    AnalyzeDependenciesOutcome {
                        crates, duration
                    }
                })
            })
        })
    }

    fn fetch_releases<I: IntoIterator<Item=CrateName>>(&self, names: I) ->
        impl Iterator<Item=impl Future<Item=Vec<CrateRelease>, Error=Error>>
    {
        let engine = self.clone();
        names.into_iter().map(move |name| {
            engine.query_crate.call(name)
                .from_err()
                .map(|resp| resp.releases.clone())
        })
    }

    fn retrieve_manifest_at_path(&self, repo_path: &RepoPath, oid: &ObjectId, path: &RelativePathBuf) ->
        impl Future<Item=String, Error=Error>
    {
        let manifest_path = path.join(RelativePath::new("Cargo.toml"));
        self.retrieve_file_at_path.call((repo_path.clone(), oid.clone(), manifest_path))
            .from_err().map(|item| item.clone())
    }

    fn find_head_oid(&self, repo_path: &RepoPath) ->
        impl Future<Item=ObjectId, Error=Error>
    {
        let url = format!("{}/{}/{}.git",
            repo_path.site.to_base_uri(),
            repo_path.qual.as_ref(),
            repo_path.name.as_ref());
        let req = LsRemoteRequest {
            https_clone_url: url
        };
        self.git_ls_remote.call(req).from_err().and_then(|refs| {
            refs.into_iter().find(|r| r.name == "HEAD")
                .map(|r| r.oid)
                .ok_or(format_err!("HEAD ref not found"))
        })
    }
}

lazy_static! {
    static ref POPULAR_REPOS_BLACKLIST: HashSet<RepoPath> = {
        vec![
            RepoPath::from_parts("github", "rust-lang", "rust"),
            RepoPath::from_parts("github", "google", "xi-editor"),
            RepoPath::from_parts("github", "lk-geimfari", "awesomo"),
            RepoPath::from_parts("github", "redox-os", "tfs"),
            RepoPath::from_parts("github", "carols10cents", "rustlings")
        ].into_iter().collect::<Result<HashSet<_>, _>>().unwrap()
    };
}
