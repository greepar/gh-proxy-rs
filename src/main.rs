use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use http::StatusCode;
use http::header::{HOST, SERVER, VIA};
use log::{error, info};
use pingora::http::ResponseHeader;
use pingora::prelude::{HttpPeer, Opt, ProxyHttp, Result, Server, Session};
use pingora::protocols::tls::ALPN;

const GITHUB_PROXY_HOST: &str = "gh.qwq.lu";
const DOCKER_PROXY_HOST: &str = "docker.qwq.lu";
const DOCKER_AUTH_PROXY_HOST: &str = "auth.docker.qwq.lu";

const GITHUB: Upstream = Upstream::new("github.com");
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

        let Some(upstream) = Self::route(host) else {
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
        peer.options.alpn = ALPN::H2H1;
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
}
