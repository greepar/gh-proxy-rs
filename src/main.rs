use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use http::header::{HOST, SERVER, VIA};
use http::{StatusCode, Uri};
use log::{error, info};
use pingora::http::ResponseHeader;
use pingora::prelude::{HttpPeer, Opt, ProxyHttp, Result, Server, Session};
use pingora::protocols::tls::ALPN;

const GITHUB_PROXY_HOST: &str = "gh.qwq.lu";
const DOCKER_PROXY_HOST: &str = "docker.qwq.lu";
const DOCKER_AUTH_PROXY_HOST: &str = "auth.docker.qwq.lu";

const GITHUB: Upstream = Upstream::new("github.com");
const GITHUB_API: Upstream = Upstream::new("api.github.com");
const GITHUB_RAW: Upstream = Upstream::new("raw.githubusercontent.com");
const GITHUB_CODELOAD: Upstream = Upstream::new("codeload.github.com");
const GITHUB_OBJECTS: Upstream = Upstream::new("objects.githubusercontent.com");
const GITHUB_RELEASES: Upstream = Upstream::new("release-assets.githubusercontent.com");
const DOCKER_REGISTRY: Upstream = Upstream::new("registry-1.docker.io");
const DOCKER_AUTH: Upstream = Upstream::new("auth.docker.io");

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(300);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(long, env = "GH_PROXY_LISTEN", default_value = "0.0.0.0:1555")]
    listen: String,
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

struct Proxy;

impl Proxy {
    fn route(host: &str) -> Option<Upstream> {
        match host
            .split(':')
            .next()
            .unwrap_or(host)
            .to_ascii_lowercase()
            .as_str()
        {
            GITHUB_PROXY_HOST => Some(GITHUB),
            DOCKER_PROXY_HOST => Some(DOCKER_REGISTRY),
            DOCKER_AUTH_PROXY_HOST => Some(DOCKER_AUTH),
            _ => None,
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

    fn github_proxy_redirect(location: &str) -> Option<String> {
        let target: Uri = location.parse().ok()?;
        Self::github_upstream(target.host()?)?;
        Some(format!("https://{GITHUB_PROXY_HOST}/{location}"))
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

        let mut upstream = Self::route(host);
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
                if let Some(rewritten) = Self::github_proxy_redirect(&location) {
                    response.insert_header("location", rewritten)?;
                    // GitHub release redirects contain short-lived signed URLs.
                    response.insert_header("cache-control", "no-store")?;
                }
            }
        }

        if ctx.upstream == Some(DOCKER_REGISTRY) {
            let challenge = response
                .headers
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if let Some(challenge) = challenge {
                response.insert_header(
                    "www-authenticate",
                    challenge.replace("https://auth.docker.io/", "https://auth.docker.qwq.lu/"),
                )?;
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
    let threads = std::thread::available_parallelism().map_or(1, usize::from);

    let conf = pingora::server::configuration::ServerConf {
        threads,
        work_stealing: true,
        upstream_keepalive_pool_size: 512,
        max_retries: 2,
        grace_period_seconds: Some(10),
        graceful_shutdown_timeout_seconds: Some(30),
        ..Default::default()
    };

    let mut server = Server::new_with_opt_and_conf(None::<Opt>, conf);
    server.bootstrap();

    let mut service = pingora::proxy::http_proxy_service(&server.configuration, Proxy);
    service.add_tcp(&args.listen);
    server.add_service(service);

    info!("listening on {}, workers={threads}", args.listen);
    server.run_forever();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_whitelisted_hosts() {
        assert_eq!(Proxy::route("gh.qwq.lu"), Some(GITHUB));
        assert_eq!(Proxy::route("gh.qwq.lu:8080"), Some(GITHUB));
        assert_eq!(Proxy::route("docker.qwq.lu"), Some(DOCKER_REGISTRY));
        assert_eq!(Proxy::route("auth.docker.qwq.lu"), Some(DOCKER_AUTH));
        assert_eq!(Proxy::route("example.com"), None);
    }

    #[test]
    fn permits_only_official_github_url_targets() {
        let target: Uri = "/https://github.com/LLOneBot/LuckyLilliaBot/releases/download/v8.1.7/LLBot-CLI-win-x64.zip?x=1"
            .parse()
            .unwrap();
        let (upstream, path) = Proxy::github_url_target(&target).unwrap();
        assert_eq!(upstream, GITHUB);
        assert_eq!(
            path.path_and_query().unwrap().as_str(),
            "/LLOneBot/LuckyLilliaBot/releases/download/v8.1.7/LLBot-CLI-win-x64.zip?x=1"
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
        assert_eq!(
            Proxy::github_proxy_redirect("https://objects.githubusercontent.com/file?token=abc")
                .as_deref(),
            Some("https://gh.qwq.lu/https://objects.githubusercontent.com/file?token=abc")
        );
        assert!(Proxy::github_proxy_redirect("https://example.com/file").is_none());
    }
}
