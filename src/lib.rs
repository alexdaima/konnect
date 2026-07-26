use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{Mutex, RwLock, watch},
};

mod config_model;
pub use config_model::{Cluster, Config, ContextsConfig, ProxyConfig, Service};

pub const DEFAULT_PROXY_PORT: u16 = 1355;

pub const CONFIG_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/schemas/konnect.config.schema.json"
));

pub const EXAMPLE_CONFIG: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.json"));

#[derive(Debug, Clone)]
pub struct ForwardSpec {
    pub route: String,
    pub context: String,
    pub namespace: String,
    pub target: String,
    pub remote_port: u16,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read configuration at {}", path.display()))?;
        let config: Self = serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.proxy.port == 0 {
            bail!("proxy.port must be between 1 and 65535");
        }
        for (name, cluster) in &self.clusters {
            if name == "all" {
                if cluster.context.is_some() {
                    bail!("clusters.all must not define a context");
                }
            } else {
                validate_label("cluster key", name)?;
                let Some(context) = &cluster.context else {
                    bail!("clusters.{name} must define a context");
                };
                if context.trim().is_empty() {
                    bail!("clusters.{name} has an empty context");
                }
            }
            validate_services(&format!("clusters.{name}"), &cluster.services)?;
        }
        Ok(())
    }

    pub fn ignored_context(&self, context: &str) -> bool {
        self.contexts
            .ignore
            .iter()
            .any(|ignored| ignored == context)
    }

    pub fn configured_context(&self, context: &str) -> bool {
        self.cluster_name_for_context(context).is_some()
    }

    pub fn cluster_name_for_context(&self, context: &str) -> Option<&str> {
        self.clusters
            .iter()
            .find(|(name, cluster)| {
                name.as_str() != "all" && cluster.context.as_deref() == Some(context)
            })
            .map(|(name, _)| name.as_str())
    }

    pub fn forwards_for_context(&self, context: &str) -> Result<Vec<ForwardSpec>> {
        let Some((cluster_name, cluster)) = self
            .clusters
            .iter()
            .find(|(name, cluster)| *name != "all" && cluster.context.as_deref() == Some(context))
        else {
            return Ok(Vec::new());
        };
        let all_services = self
            .clusters
            .get("all")
            .map_or(&[][..], |cluster| cluster.services.as_slice());
        let all_service_names: HashSet<String> = all_services
            .iter()
            .map(Service::route_name)
            .collect::<Result<_>>()?;
        let mut overrides = HashMap::new();
        for service in &cluster.services {
            overrides.insert(service.route_name()?, service);
        }

        let mut forwards = Vec::new();
        for service in all_services {
            let name = service.route_name()?;
            let service = overrides.get(&name).copied().unwrap_or(service);
            forwards.push(forward_spec(cluster_name, context, service)?);
        }
        for service in &cluster.services {
            let name = service.route_name()?;
            if !all_service_names.contains(&name) {
                forwards.push(forward_spec(cluster_name, context, service)?);
            }
        }
        Ok(forwards)
    }

    pub fn forwards_for_contexts(&self, contexts: &[String]) -> Result<Vec<ForwardSpec>> {
        let mut forwards = Vec::new();
        let mut routes = HashMap::new();
        for context in contexts {
            if self.ignored_context(context) {
                continue;
            }
            for forward in self.forwards_for_context(context)? {
                if let Some(previous) =
                    routes.insert(forward.route.clone(), forward.context.clone())
                {
                    bail!(
                        "contexts {previous:?} and {:?} produce the same route {}",
                        forward.context,
                        forward.route
                    );
                }
                forwards.push(forward);
            }
        }
        Ok(forwards)
    }
}

fn validate_services(scope: &str, services: &[Service]) -> Result<()> {
    let mut names = HashMap::new();
    for service in services {
        let name = service.route_name()?;
        validate_label("service name", &name)?;
        if service.namespace.trim().is_empty() {
            bail!("{scope} service {name} has an empty namespace");
        }
        if service.remote_port == 0 {
            bail!("{scope} service {name} has an invalid remote_port");
        }
        service.target_name()?;
        if names.insert(name.clone(), ()).is_some() {
            bail!("{scope} defines service {name} more than once");
        }
    }
    Ok(())
}

fn forward_spec(cluster_name: &str, context: &str, service: &Service) -> Result<ForwardSpec> {
    Ok(ForwardSpec {
        route: route_name(cluster_name, &service.route_name()?),
        context: context.to_owned(),
        namespace: service.namespace.clone(),
        target: service.target_name()?,
        remote_port: service.remote_port,
    })
}

impl Service {
    fn route_name(&self) -> Result<String> {
        if let Some(name) = &self.name {
            return Ok(name.clone());
        }
        let target = self.target_name()?;
        let Some((_, name)) = target.rsplit_once('/') else {
            bail!("target {target:?} must include a resource name; set service.name explicitly");
        };
        if name.is_empty() {
            bail!("target {target:?} has an empty resource name");
        }
        Ok(name.to_owned())
    }

    fn target_name(&self) -> Result<String> {
        let configured = [
            self.target.as_ref(),
            self.service.as_ref(),
            self.pod.as_ref(),
        ]
        .iter()
        .filter(|value| value.is_some())
        .count();
        if configured != 1 {
            bail!("service must define exactly one of target, service, or pod");
        }
        if let Some(target) = &self.target {
            if target.trim().is_empty() {
                bail!("service has an empty target");
            }
            return Ok(target.clone());
        }
        if let Some(service) = &self.service {
            if service.trim().is_empty() {
                bail!("service has an empty service target");
            }
            return Ok(format!("svc/{service}"));
        }
        let pod = self.pod.as_ref().expect("validated target configuration");
        if pod.trim().is_empty() {
            bail!("service has an empty pod target");
        }
        Ok(format!("pod/{pod}"))
    }
}

fn validate_label(field: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-');
    if !valid {
        bail!("{field} must be a lowercase DNS label (letters, numbers, and hyphens)");
    }
    Ok(())
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("KONNECT_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .context("HOME is not set; use KONNECT_CONFIG to set a config path")?;
    Ok(PathBuf::from(home).join(".konnect").join("config.json"))
}

pub fn kube_contexts() -> Result<Vec<String>> {
    let kubectl = kubectl_path()?;
    let output = std::process::Command::new(&kubectl)
        .args(["config", "get-contexts", "--output=name"])
        .output()
        .with_context(|| format!("failed to run {}", kubectl.display()))?;
    if !output.status.success() {
        bail!(
            "kubectl config get-contexts failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut contexts: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|context| !context.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    contexts.sort();
    contexts.dedup();
    if contexts.is_empty() {
        bail!("kubectl did not return any kube contexts");
    }
    Ok(contexts)
}

fn kubectl_path() -> Result<PathBuf> {
    let executable = if cfg!(target_os = "windows") {
        "kubectl.exe"
    } else {
        "kubectl"
    };
    let mut directories = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    if let Some(path) = env::var_os("PATH") {
        directories.extend(env::split_paths(&path));
    }
    directories.sort();
    directories.dedup();
    for directory in directories {
        let candidate = directory.join(executable);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("kubectl was not found in PATH")
}

pub fn route_name(cluster: &str, service: &str) -> String {
    format!("{cluster}.{service}")
}

pub fn browser_host(route: &str) -> String {
    format!("{route}.localhost")
}

pub fn canonical_route(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(&host);
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(host, |(name, _)| name);
    host.strip_suffix(".localhost").unwrap_or(host).to_owned()
}

pub fn parse_forwarded_port(line: &str) -> Option<u16> {
    let address = line.strip_prefix("Forwarding from ")?.split_once(" -> ")?.0;
    address.rsplit_once(':')?.1.parse().ok()
}

#[derive(Default)]
struct RouteState {
    port: Option<u16>,
}

type Routes = Arc<RwLock<HashMap<String, Arc<RwLock<RouteState>>>>>;

#[derive(Clone)]
pub struct ForwardManager {
    routes: Routes,
    active: Arc<Mutex<HashMap<String, ActiveForward>>>,
}

struct ActiveForward {
    shutdown: watch::Sender<bool>,
    state: Arc<RwLock<RouteState>>,
}

pub struct KonnectRuntime {
    manager: ForwardManager,
    proxy_shutdown: watch::Sender<bool>,
    transfer_stats: TransferStats,
}

#[derive(Clone, Default)]
pub struct TransferStats {
    sent: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
}

impl TransferStats {
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.sent.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed),
        )
    }
}

impl KonnectRuntime {
    pub fn manager(&self) -> ForwardManager {
        self.manager.clone()
    }

    pub fn transfer_stats(&self) -> TransferStats {
        self.transfer_stats.clone()
    }

    pub async fn shutdown(self) {
        self.manager.stop_all().await;
        let _ = self.proxy_shutdown.send(true);
    }
}

impl ForwardManager {
    pub async fn toggle(&self, spec: ForwardSpec) -> Result<bool> {
        let mut active = self.active.lock().await;
        if let Some(forward) = active.remove(&spec.route) {
            let _ = forward.shutdown.send(true);
            forward.state.write().await.port = None;
            return Ok(false);
        }

        let state = Arc::new(RwLock::new(RouteState::default()));
        self.routes
            .write()
            .await
            .insert(spec.route.clone(), state.clone());
        let (shutdown, shutdown_rx) = watch::channel(false);
        active.insert(
            spec.route.clone(),
            ActiveForward {
                shutdown,
                state: state.clone(),
            },
        );
        tokio::spawn(run_forward(spec, state, shutdown_rx));
        Ok(true)
    }

    pub async fn stop_all(&self) {
        let mut active = self.active.lock().await;
        for (_, forward) in active.drain() {
            let _ = forward.shutdown.send(true);
            forward.state.write().await.port = None;
        }
    }
}

pub async fn start_runtime(forwards: &[ForwardSpec], proxy_port: u16) -> Result<KonnectRuntime> {
    let listener = TcpListener::bind(("127.0.0.1", proxy_port))
        .await
        .with_context(|| format!("could not bind the Konnect proxy on 127.0.0.1:{proxy_port}"))?;
    let routes: Routes = Arc::new(RwLock::new(HashMap::new()));
    for forward in forwards {
        routes.write().await.insert(
            forward.route.clone(),
            Arc::new(RwLock::new(RouteState::default())),
        );
    }

    let (shutdown, shutdown_rx) = watch::channel(false);
    let transfer_stats = TransferStats::default();
    tokio::spawn(run_proxy(
        listener,
        routes.clone(),
        shutdown_rx,
        transfer_stats.clone(),
    ));
    Ok(KonnectRuntime {
        manager: ForwardManager {
            routes,
            active: Arc::new(Mutex::new(HashMap::new())),
        },
        proxy_shutdown: shutdown,
        transfer_stats,
    })
}

async fn run_forward(
    spec: ForwardSpec,
    state: Arc<RwLock<RouteState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut retry_delay = Duration::from_secs(1);
    loop {
        if *shutdown.borrow() {
            return;
        }
        match start_forward(&spec, state.clone(), &mut shutdown).await {
            Ok(()) => return,
            Err(error) => eprintln!(
                "{}: {error}; retrying in {}s",
                spec.route,
                retry_delay.as_secs()
            ),
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(retry_delay) => {},
        }
        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
    }
}

async fn start_forward(
    spec: &ForwardSpec,
    state: Arc<RwLock<RouteState>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
    let kubectl = kubectl_path()?;
    let mut child = Command::new(kubectl)
        .args([
            "--context",
            &spec.context,
            "port-forward",
            "--address",
            "127.0.0.1",
            "--namespace",
            &spec.namespace,
            &spec.target,
            &format!("0:{}", spec.remote_port),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| "failed to start kubectl; ensure it is installed and on PATH")?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let output_state = state.clone();
    let stdout_task = tokio::spawn(read_forward_output(
        spec.route.clone(),
        stdout,
        output_state,
    ));
    let stderr_task = tokio::spawn(read_forward_output(
        spec.route.clone(),
        stderr,
        state.clone(),
    ));

    let exit = tokio::select! {
        _ = shutdown.changed() => {
            child.start_kill().context("failed to stop kubectl")?;
            child.wait().await.context("failed waiting for kubectl")?
        }
        status = child.wait() => status.context("failed waiting for kubectl")?,
    };
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    state.write().await.port = None;
    if *shutdown.borrow() {
        return Ok(());
    }
    bail!("kubectl port-forward exited with {exit}")
}

async fn read_forward_output<R>(route: String, reader: R, state: Arc<RwLock<RouteState>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(port) = parse_forwarded_port(&line) {
            state.write().await.port = Some(port);
            eprintln!("{route}: connected on 127.0.0.1:{port}");
        } else {
            eprintln!("{route}: {line}");
        }
    }
}

async fn run_proxy(
    listener: TcpListener,
    routes: Routes,
    mut shutdown: watch::Receiver<bool>,
    transfer_stats: TransferStats,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, address) = accepted.context("failed to accept proxy connection")?;
                let routes = routes.clone();
                let transfer_stats = transfer_stats.clone();
                tokio::spawn(async move {
                    if let Err(error) = proxy_connection(stream, routes, transfer_stats).await {
                        eprintln!("proxy {address}: {error}");
                    }
                });
            }
        }
    }
}

async fn proxy_connection(
    mut client: TcpStream,
    routes: Routes,
    transfer_stats: TransferStats,
) -> Result<()> {
    let request = read_request_headers(&mut client).await?;
    let host = request_host(&request).context("request does not include a Host header")?;
    let route = canonical_route(host);
    let state = routes.read().await.get(&route).cloned();
    let Some(state) = state else {
        return respond(&mut client, 404, "Unknown Konnect route").await;
    };
    let Some(port) = state.read().await.port else {
        return respond(&mut client, 503, "Konnect route is still connecting").await;
    };
    let mut upstream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .with_context(|| format!("route {route} is unavailable"))?;
    upstream
        .write_all(&request)
        .await
        .context("failed to send request to upstream")?;
    transfer_stats
        .sent
        .fetch_add(request.len() as u64, Ordering::Relaxed);
    let mut client = CountingStream::new(client, transfer_stats.sent.clone());
    let mut upstream = CountingStream::new(upstream, transfer_stats.received.clone());
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .context("proxy connection failed")?;
    Ok(())
}

struct CountingStream {
    inner: TcpStream,
    read_counter: Arc<AtomicU64>,
}

impl CountingStream {
    fn new(inner: TcpStream, read_counter: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            read_counter,
        }
    }
}

impl AsyncRead for CountingStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match std::pin::Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                self.read_counter
                    .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for CountingStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn read_request_headers(client: &mut TcpStream) -> Result<Vec<u8>> {
    const MAX_HEADER_BYTES: usize = 32 * 1024;
    let mut request = Vec::with_capacity(1024);
    loop {
        let mut chunk = [0_u8; 1024];
        let count = client
            .read(&mut chunk)
            .await
            .context("failed to read request")?;
        if count == 0 {
            bail!("client closed connection before sending headers");
        }
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
        if request.len() > MAX_HEADER_BYTES {
            bail!("request headers exceed {MAX_HEADER_BYTES} bytes");
        }
    }
}

fn request_host(request: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(request).ok()?;
    text.split("\r\n").skip(1).find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.trim())
    })
}

async fn respond(client: &mut TcpStream, status: u16, message: &str) -> Result<()> {
    let response =
        format!("HTTP/1.1 {status} {message}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    client
        .write_all(response.as_bytes())
        .await
        .context("failed to write proxy response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_example_config_is_valid() {
        let config: Config = serde_json::from_str(EXAMPLE_CONFIG).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn accepts_service_and_pod_targets() {
        let config: Config = serde_json::from_str(
            r#"{
                "clusters": {
                    "dev": {
                        "context": "kind-dev",
                        "service": [
                            {"name": "api", "namespace": "default", "service": "api", "remote_port": 8080},
                            {"name": "worker", "namespace": "default", "pod": "worker-123", "remote_port": 9000}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
        let forwards = config.forwards_for_context("kind-dev").unwrap();
        assert_eq!(forwards[0].route, "dev.api");
        assert_eq!(forwards[0].target, "svc/api");
        assert_eq!(forwards[1].target, "pod/worker-123");
    }

    #[test]
    fn rejects_ambiguous_targets() {
        let config: Config = serde_json::from_str(
            r#"{
                "clusters": {
                    "dev": {
                        "context": "kind-dev",
                        "service": [
                            {"name": "api", "namespace": "default", "service": "api", "pod": "api-123", "remote_port": 8080}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("exactly one")
        );
    }

    #[test]
    fn identifies_configured_kube_contexts() {
        let config: Config = serde_json::from_str(
            r#"{
                "contexts": {"ignore": ["docker-desktop"]},
                "clusters": {
                    "staging": {
                        "context": "arn:example:staging",
                        "service": [
                            {"name": "grafana", "namespace": "observability", "service": "grafana", "remote_port": 3000}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.forwards_for_context("arn:example:staging").unwrap()[0].route,
            "staging.grafana"
        );
        assert!(config.configured_context("arn:example:staging"));
        assert!(!config.configured_context("kind-dev"));
        assert_eq!(
            config.cluster_name_for_context("arn:example:staging"),
            Some("staging")
        );
        assert!(config.forwards_for_context("kind-dev").unwrap().is_empty());
        assert!(config.ignored_context("docker-desktop"));
    }

    #[test]
    fn expands_all_services_and_allows_context_overrides() {
        let config: Config = serde_json::from_str(
            r#"{
                "clusters": {
                    "all": {
                        "service": [
                            {"name": "api", "namespace": "default", "service": "api", "remote_port": 8080},
                            {"name": "metrics", "namespace": "observability", "service": "metrics-server", "remote_port": 443}
                        ]
                    },
                    "staging": {
                        "context": "team-staging",
                        "service": [
                            {"name": "api", "namespace": "staging", "pod": "api-123", "remote_port": 9000}
                        ]
                    },
                    "kind": {"context": "kind-dev"}
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();

        let staging = config.forwards_for_context("team-staging").unwrap();
        assert_eq!(staging[0].route, "staging.api");
        assert_eq!(staging[0].target, "pod/api-123");
        assert_eq!(staging[1].route, "staging.metrics");

        let other = config.forwards_for_context("kind-dev").unwrap();
        assert_eq!(other[0].route, "kind.api");
        assert_eq!(other[0].target, "svc/api");
        assert_eq!(other[1].route, "kind.metrics");
    }

    #[test]
    fn derives_a_service_name_from_the_target() {
        let config: Config = serde_json::from_str(
            r#"{
                "clusters": {
                    "usa": {
                        "context": "my-usa-kube-context",
                        "service": [
                            {"namespace": "observability", "target": "svc/grafana", "remote_port": 3000}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.forwards_for_context("my-usa-kube-context").unwrap()[0].route,
            "usa.grafana"
        );
    }

    #[test]
    fn normalizes_browser_hosts_to_route_names() {
        assert_eq!(
            canonical_route("Staging.Grafana.localhost:1355"),
            "staging.grafana"
        );
        assert_eq!(canonical_route("staging.grafana"), "staging.grafana");
    }

    #[test]
    fn parses_kubectl_assigned_port() {
        assert_eq!(
            parse_forwarded_port("Forwarding from 127.0.0.1:49152 -> 8080"),
            Some(49152)
        );
    }

    #[tokio::test]
    async fn proxies_a_localhost_route_to_its_upstream() {
        const REQUEST: &[u8] =
            b"GET / HTTP/1.1\r\nHost: dev.api.localhost\r\nConnection: close\r\n\r\n";
        const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
        let upstream_listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind upstream listener: {error}"),
        };
        let upstream_port = upstream_listener.local_addr().unwrap().port();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.unwrap();
            let mut request = [0_u8; 512];
            let count = stream.read(&mut request).await.unwrap();
            assert!(
                std::str::from_utf8(&request[..count])
                    .unwrap()
                    .contains("Host: dev.api.localhost")
            );
            stream.write_all(RESPONSE).await.unwrap();
        });

        let state = Arc::new(RwLock::new(RouteState {
            port: Some(upstream_port),
        }));
        let routes: Routes = Arc::new(RwLock::new(HashMap::from([("dev.api".to_owned(), state)])));
        let proxy_listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind proxy listener: {error}"),
        };
        let proxy_address = proxy_listener.local_addr().unwrap();
        let transfer_stats = TransferStats::default();
        let proxy_transfer_stats = transfer_stats.clone();
        let proxy = tokio::spawn(async move {
            let (stream, _) = proxy_listener.accept().await.unwrap();
            proxy_connection(stream, routes, proxy_transfer_stats)
                .await
                .unwrap();
        });

        let mut client = TcpStream::connect(proxy_address).await.unwrap();
        client.write_all(REQUEST).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();

        assert!(
            std::str::from_utf8(&response)
                .unwrap()
                .ends_with("\r\n\r\nOK")
        );
        upstream.await.unwrap();
        proxy.await.unwrap();
        assert_eq!(
            transfer_stats.snapshot(),
            (REQUEST.len() as u64, RESPONSE.len() as u64)
        );
    }
}
