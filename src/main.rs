use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use http::header::{HOST, SERVER, VIA};
use http::{StatusCode, Uri};
use log::{error, info};
use pingora::http::ResponseHeader;
use pingora::prelude::{HttpPeer, Opt, ProxyHttp, Result, Server, Session};
use pingora::protocols::tls::ALPN;

const GITHUB: Upstream = Upstream::new("github.com");
const GITHUB_API: Upstream = Upstream::new("api.github.com");
const GITHUB_RAW: Upstream = Upstream::new("raw.githubusercontent.com");
const GITHUB_CODELOAD: Upstream = Upstream::new("codeload.github.com");
const GITHUB_OBJECTS: Upstream = Upstream::new("objects.githubusercontent.com");
const GITHUB_RELEASES: Upstream = Upstream::new("release-assets.githubusercontent.com");
const DOCKER_REGISTRY: Upstream = Upstream::new("registry-1.docker.io");
const DOCKER_AUTH: Upstream = Upstream::new("auth.docker.io");
const GHCR: Upstream = Upstream::new("ghcr.io");

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(300);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(long, env = "GH_PROXY_LISTEN", default_value = "0.0.0.0:1555")]
    listen: String,

    #[arg(long, env = "GH_PROXY_GITHUB_HOST")]
    github_host: String,

    #[arg(long, env = "GH_PROXY_DOCKER_HOST")]
    docker_host: Option<String>,

    #[arg(long, env = "GH_PROXY_DOCKER_AUTH_HOST")]
    docker_auth_host: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Upstream {
    host: &'static str,
}

impl Upstream {
    const fn new(host: &'static str) -> Self {
        Self { host }
    }

    fn address(self) -> String {
        format!("{}:443", self.host)
    }
}

#[derive(Default)]
struct RequestContext {
    upstream: Option<Upstream>,
}

struct Proxy {
    github_host: String,
    docker_host: Option<String>,
    docker_auth_host: Option<String>,
}

impl Proxy {
    fn configured_host(host: &str) -> String {
        host.split(':')
            .next()
            .unwrap_or(host)
            .trim()
            .to_ascii_lowercase()
    }

    fn route(&self, host: &str) -> Option<Upstream> {
        let host = Self::configured_host(host);
        if host == self.github_host {
            Some(GITHUB)
        } else if self.docker_host.as_deref() == Some(host.as_str()) {
            Some(DOCKER_REGISTRY)
        } else if self.docker_auth_host.as_deref() == Some(host.as_str()) {
            Some(DOCKER_AUTH)
        } else {
            None
        }
    }

    fn github_upstream(host: &str) -> Option<Upstream> {
        match host.to_ascii_lowercase().as_str() {
            "github.com" => Some(GITHUB),
            "api.github.com" => Some(GITHUB_API),
            "raw.githubusercontent.com" => Some(GITHUB_RAW),
            "codeload.github.com" => Some(GITHUB_CODELOAD),
            "objects.githubusercontent.com" => Some(GITHUB_OBJECTS),
            "release-assets.githubusercontent.com" => Some(GITHUB_RELEASES),
            _ => None,
        }
    }

    fn github_url_target(uri: &Uri) -> Option<(Upstream, Uri)> {
        let target = uri.path_and_query()?.as_str().strip_prefix('/')?;
        let target = target
            .strip_prefix("https://")
            .or_else(|| target.strip_prefix("http://"))
            .unwrap_or(target);
        let target: Uri = format!("https://{target}").parse().ok()?;
        let upstream = Self::github_upstream(target.host()?)?;
        let path_and_query = target.path_and_query().map_or("/", |value| value.as_str());
        Some((upstream, path_and_query.parse().ok()?))
    }

    fn is_github_url_format(uri: &Uri) -> bool {
        let target = uri.path().strip_prefix('/').unwrap_or_default();
        if target.starts_with("https://") || target.starts_with("http://") {
            return true;
        }

        target
            .split('/')
            .next()
            .is_some_and(|host| Self::github_upstream(host).is_some())
    }

    fn github_proxy_redirect(&self, location: &str) -> Option<String> {
        let target: Uri = location.parse().ok()?;
        Self::github_upstream(target.host()?)?;
        Some(format!("https://{}/{location}", self.github_host))
    }

    fn ghcr_target(uri: &Uri) -> Option<Uri> {
        let target = uri.path_and_query()?.as_str();
        let target = if let Some(target) = target.strip_prefix("/v2/ghcr.io") {
            format!("/v2{target}")
        } else {
            let target = target.strip_prefix("/ghcr.io")?;
            target.to_owned()
        };

        let target = if target.is_empty() || target.starts_with('?') {
            format!("/{target}")
        } else {
            target
        };
        target.parse().ok()
    }

    async fn write_status(
        session: &mut Session,
        status: StatusCode,
        body: &'static [u8],
    ) -> Result<()> {
        let mut response = ResponseHeader::build(status, Some(4))?;
        response.insert_header("content-type", "text/plain; charset=utf-8")?;
        response.insert_header("content-length", body.len().to_string())?;
        response.insert_header("cache-control", "no-store")?;
        session
            .write_response_header(Box::new(response), false)
            .await?;
        session.write_response_body(Some(body.into()), true).await
    }
}

#[async_trait]
impl ProxyHttp for Proxy {
    type CTX = RequestContext;

    fn new_ctx(&self) -> Self::CTX {
        RequestContext::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        if session.req_header().uri.path() == "/healthz" {
            Self::write_status(session, StatusCode::OK, b"ok\n").await?;
            return Ok(true);
        }

        let host = session
            .req_header()
            .headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();

        let mut upstream = self.route(host);
        if upstream == Some(DOCKER_REGISTRY) {
            if let Some(target_uri) = Self::ghcr_target(&session.req_header().uri) {
                session.req_header_mut().set_uri(target_uri);
                upstream = Some(GHCR);
            }
        }

        if upstream == Some(GITHUB) && Self::is_github_url_format(&session.req_header().uri) {
            let Some((target_upstream, target_uri)) =
                Self::github_url_target(&session.req_header().uri)
            else {
                Self::write_status(
                    session,
                    StatusCode::BAD_REQUEST,
                    b"github URL not allowed\n",
                )
                .await?;
                return Ok(true);
            };
            session.req_header_mut().set_uri(target_uri);
            upstream = Some(target_upstream);
        }

        let Some(upstream) = upstream else {
            Self::write_status(
                session,
                StatusCode::MISDIRECTED_REQUEST,
                b"host not allowed\n",
            )
            .await?;
            return Ok(true);
        };

        ctx.upstream = Some(upstream);
        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let upstream = ctx.upstream.expect("request_filter sets upstream");
        let mut peer = HttpPeer::new(upstream.address(), true, upstream.host.to_owned());
        peer.options.connection_timeout = Some(CONNECT_TIMEOUT);
        peer.options.total_connection_timeout = Some(CONNECT_TIMEOUT);
        peer.options.read_timeout = Some(IO_TIMEOUT);
        peer.options.write_timeout = Some(IO_TIMEOUT);
        peer.options.idle_timeout = Some(IDLE_TIMEOUT);
        peer.options.alpn = ALPN::H1;
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        request: &mut pingora::http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        let upstream = ctx.upstream.expect("request_filter sets upstream");
        request.insert_header(HOST, upstream.host)?;
        request.remove_header(&VIA);
        request.remove_header("x-forwarded-host");
        request.remove_header("x-forwarded-server");
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        response.remove_header(&SERVER);
        response.remove_header(&VIA);

        if ctx
            .upstream
            .is_some_and(|upstream| upstream != DOCKER_REGISTRY)
        {
            let location = response
                .headers
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if let Some(location) = location {
                if let Some(rewritten) = self.github_proxy_redirect(&location) {
                    response.insert_header("location", rewritten)?;
                    // GitHub release redirects contain short-lived signed URLs.
                    response.insert_header("cache-control", "no-store")?;
                }
            }
        }

        if ctx.upstream == Some(DOCKER_REGISTRY) || ctx.upstream == Some(GHCR) {
            let challenge = response
                .headers
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if let Some(challenge) = challenge {
                let challenge = if ctx.upstream == Some(GHCR) {
                    challenge.replace(
                        "https://ghcr.io/token",
                        &format!(
                            "https://{}/ghcr.io/token",
                            self.docker_host
                                .as_deref()
                                .expect("GHCR requires Docker host")
                        ),
                    )
                } else if let Some(docker_auth_host) = self.docker_auth_host.as_deref() {
                    challenge.replace(
                        "https://auth.docker.io/",
                        &format!("https://{docker_auth_host}/"),
                    )
                } else {
                    challenge
                };
                response.insert_header("www-authenticate", challenge)?;
            }
        }

        Ok(())
    }

    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        if let Some(error) = error {
            error!(
                "proxy error upstream={} path={} error={error}",
                ctx.upstream.map_or("none", |upstream| upstream.host),
                session.req_header().uri.path()
            );
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();
    let docker_host = args
        .docker_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(Proxy::configured_host);
    let docker_auth_host = args
        .docker_auth_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(Proxy::configured_host);
    let threads = std::thread::available_parallelism().map_or(1, usize::from);

    let conf = pingora::server::configuration::ServerConf {
        threads,
        work_stealing: true,
        upstream_keepalive_pool_size: 512,
        max_retries: 2,
        ..Default::default()
    };

    let mut server = Server::new_with_opt_and_conf(None::<Opt>, conf);
    server.bootstrap();

    let proxy = Proxy {
        github_host: Proxy::configured_host(&args.github_host),
        docker_host,
        docker_auth_host,
    };
    let mut service = pingora::proxy::http_proxy_service(&server.configuration, proxy);
    service.add_tcp(&args.listen);
    server.add_service(service);

    info!("listening on {}, workers={threads}", args.listen);
    server.run_forever();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> Proxy {
        Proxy {
            github_host: "github-proxy.example.com".to_owned(),
            docker_host: Some("docker-proxy.example.com".to_owned()),
            docker_auth_host: Some("docker-auth.example.com".to_owned()),
        }
    }

    #[test]
    fn routes_only_whitelisted_hosts() {
        let proxy = proxy();
        assert_eq!(proxy.route("github-proxy.example.com"), Some(GITHUB));
        assert_eq!(proxy.route("github-proxy.example.com:8080"), Some(GITHUB));
        assert_eq!(
            proxy.route("docker-proxy.example.com"),
            Some(DOCKER_REGISTRY)
        );
        assert_eq!(proxy.route("docker-auth.example.com"), Some(DOCKER_AUTH));
        assert_eq!(proxy.route("example.com"), None);
    }

    #[test]
    fn permits_disabling_optional_proxy_hosts_independently() {
        let registry_only = Proxy {
            github_host: "github-proxy.example.com".to_owned(),
            docker_host: Some("docker-proxy.example.com".to_owned()),
            docker_auth_host: None,
        };
        assert_eq!(
            registry_only.route("docker-proxy.example.com"),
            Some(DOCKER_REGISTRY)
        );
        assert_eq!(registry_only.route("docker-auth.example.com"), None);

        let github_only = Proxy {
            github_host: "github-proxy.example.com".to_owned(),
            docker_host: None,
            docker_auth_host: None,
        };
        assert_eq!(github_only.route("github-proxy.example.com"), Some(GITHUB));
        assert_eq!(github_only.route("docker-proxy.example.com"), None);
    }

    #[test]
    fn permits_only_official_github_url_targets() {
        let target: Uri = "/https://github.com/llvm/llvm-project/releases/download/llvmorg-22.1.8/LLVM-22.1.8-Linux-X64.tar.xz"
            .parse()
            .unwrap();
        let (upstream, path) = Proxy::github_url_target(&target).unwrap();
        assert_eq!(upstream, GITHUB);
        assert_eq!(
            path.path_and_query().unwrap().as_str(),
            "/llvm/llvm-project/releases/download/llvmorg-22.1.8/LLVM-22.1.8-Linux-X64.tar.xz"
        );

        let blocked: Uri = "/https://example.com/file".parse().unwrap();
        assert!(Proxy::github_url_target(&blocked).is_none());

        let raw: Uri =
            "/raw.githubusercontent.com/komari-monitor/komari-agent/refs/heads/main/install.sh"
                .parse()
                .unwrap();
        let (upstream, path) = Proxy::github_url_target(&raw).unwrap();
        assert_eq!(upstream, GITHUB_RAW);
        assert_eq!(
            path.path(),
            "/komari-monitor/komari-agent/refs/heads/main/install.sh"
        );

        let http: Uri = "/http://raw.githubusercontent.com/komari-monitor/komari-agent/refs/heads/main/install.sh"
            .parse()
            .unwrap();
        assert_eq!(Proxy::github_url_target(&http).unwrap().0, GITHUB_RAW);
    }

    #[test]
    fn rewrites_github_download_redirects_through_proxy() {
        let proxy = proxy();
        assert_eq!(
            proxy
                .github_proxy_redirect("https://objects.githubusercontent.com/file?token=abc")
                .as_deref(),
            Some(
                "https://github-proxy.example.com/https://objects.githubusercontent.com/file?token=abc"
            )
        );
        assert!(
            proxy
                .github_proxy_redirect("https://example.com/file")
                .is_none()
        );
    }

    #[test]
    fn routes_ghcr_namespace_through_ghcr() {
        let image: Uri = "/v2/ghcr.io/greepar/gh-proxy-rs/manifests/latest"
            .parse()
            .unwrap();
        assert_eq!(
            Proxy::ghcr_target(&image)
                .unwrap()
                .path_and_query()
                .unwrap()
                .as_str(),
            "/v2/greepar/gh-proxy-rs/manifests/latest"
        );

        let token: Uri = "/ghcr.io/token?service=ghcr.io".parse().unwrap();
        assert_eq!(
            Proxy::ghcr_target(&token)
                .unwrap()
                .path_and_query()
                .unwrap()
                .as_str(),
            "/token?service=ghcr.io"
        );

        let docker_hub: Uri = "/v2/library/alpine/manifests/latest".parse().unwrap();
        assert!(Proxy::ghcr_target(&docker_hub).is_none());
    }
}
