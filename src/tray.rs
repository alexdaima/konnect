use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io::Write,
    process::{Command as ProcessCommand, Stdio},
    rc::Rc,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use konnect::{
    Config, ForwardSpec, KonnectRuntime, TransferStats, browser_host, kube_contexts, start_runtime,
};
use system_status_bar_macos::{LoopTerminator, Menu, MenuItem, StatusItem, sync_event_loop};
use tokio::runtime::Runtime;

const STATUS_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/logo.png"
));
const STATUS_ICON_FALLBACK: &str = "\u{2388}";
const TRANSFER_STATS_ITEM_INDEX: usize = 1;

enum Command {
    Toggle(String),
    CopyUrl(String),
    UpdateTransferStats,
    RefreshContexts,
    StopAll,
    Quit,
}

struct TrayApp {
    config: Config,
    contexts: Vec<String>,
    runtime: Runtime,
    konnect: Option<KonnectRuntime>,
    status_item: Option<StatusItem>,
    routes: HashMap<String, ForwardSpec>,
    active_routes: HashSet<String>,
    transfer_stats: TransferStats,
    commands: Sender<Command>,
}

impl TrayApp {
    fn install_menu(&mut self) {
        self.routes.clear();
        let menu = self.build_menu();
        if let Some(status_item) = &mut self.status_item {
            status_item.set_menu(menu);
        } else {
            let mut status_item = StatusItem::new("", menu);
            if !status_item.set_image_data(STATUS_ICON) {
                status_item.set_title(STATUS_ICON_FALLBACK);
            }
            self.status_item = Some(status_item);
        }
    }

    fn build_menu(&mut self) -> Menu {
        let mut items = Vec::new();
        items.push(MenuItem::new(
            "Tools",
            None,
            Some(Menu::new(vec![
                MenuItem::new("kubectl", None, None),
                MenuItem::new(
                    format!("Proxy: 127.0.0.1:{}", self.config.proxy.port),
                    None,
                    None,
                ),
            ])),
        ));
        items.push(MenuItem::new(
            transfer_label(&self.transfer_stats),
            None,
            None,
        ));

        for context in &self.contexts {
            if self.config.ignored_context(context) {
                continue;
            }
            let forwards = self
                .config
                .forwards_for_context(context)
                .expect("configuration was validated before creating the menu");
            let submenu = if forwards.is_empty() {
                Menu::new(vec![MenuItem::new("No configured services", None, None)])
            } else {
                Menu::new(
                    forwards
                        .into_iter()
                        .map(|forward| {
                            let route = forward.route.clone();
                            self.routes.insert(route.clone(), forward);
                            let commands = self.commands.clone();
                            MenuItem::new(
                                service_label(&route, self.active_routes.contains(&route)),
                                Some(Box::new(move || {
                                    let _ = commands.send(Command::Toggle(route.clone()));
                                })),
                                None,
                            )
                        })
                        .collect(),
                )
            };
            items.push(MenuItem::new(
                context_label(&self.config, context),
                None,
                Some(submenu),
            ));
        }

        items.push(separator());
        let commands = self.commands.clone();
        items.push(MenuItem::new(
            "Refresh Contexts",
            Some(Box::new(move || {
                let _ = commands.send(Command::RefreshContexts);
            })),
            None,
        ));
        let commands = self.commands.clone();
        items.push(MenuItem::new(
            "Stop All",
            if self.active_routes.is_empty() {
                None
            } else {
                Some(Box::new(move || {
                    let _ = commands.send(Command::StopAll);
                }))
            },
            None,
        ));
        let commands = self.commands.clone();
        items.push(MenuItem::new(
            "Quit",
            Some(Box::new(move || {
                let _ = commands.send(Command::Quit);
            })),
            None,
        ));
        items.push(separator());
        items.push(MenuItem::new("Active Forwards", None, None));

        if self.active_routes.is_empty() {
            items.push(MenuItem::new("No active forwards", None, None));
        } else {
            let mut routes: Vec<_> = self.active_routes.iter().collect();
            routes.sort();
            items.extend(routes.into_iter().map(|route| {
                let route = route.to_owned();
                let url = active_url(&route, self.config.proxy.port);
                let copy_url = url.clone();
                let copy_commands = self.commands.clone();
                let stop_route = route.clone();
                let stop_commands = self.commands.clone();
                MenuItem::new(
                    format!("{route} - {url}"),
                    None,
                    Some(Menu::new(vec![
                        MenuItem::new(
                            "Copy URL to Clipboard",
                            Some(Box::new(move || {
                                let _ = copy_commands.send(Command::CopyUrl(copy_url.clone()));
                            })),
                            None,
                        ),
                        MenuItem::new(
                            "Stop",
                            Some(Box::new(move || {
                                let _ = stop_commands.send(Command::Toggle(stop_route.clone()));
                            })),
                            None,
                        ),
                    ])),
                )
            }));
        }
        Menu::new(items)
    }

    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Toggle(route) => {
                let Some(forward) = self.routes.get(&route).cloned() else {
                    return false;
                };
                let Some(konnect) = &self.konnect else {
                    return false;
                };
                match self.runtime.block_on(konnect.manager().toggle(forward)) {
                    Ok(true) => {
                        self.active_routes.insert(route);
                        self.install_menu();
                    }
                    Ok(false) => {
                        self.active_routes.remove(&route);
                        self.install_menu();
                    }
                    Err(error) => eprintln!("failed to toggle {route}: {error:#}"),
                }
            }
            Command::CopyUrl(url) => {
                if let Err(error) = copy_to_clipboard(&url) {
                    eprintln!("failed to copy URL: {error:#}");
                }
            }
            Command::UpdateTransferStats => self.update_transfer_stats(),
            Command::RefreshContexts => match kube_contexts() {
                Ok(contexts) => {
                    self.contexts = configured_contexts(&self.config, contexts);
                    self.install_menu();
                }
                Err(error) => eprintln!("failed to refresh kube contexts: {error:#}"),
            },
            Command::StopAll => {
                if let Some(konnect) = &self.konnect {
                    self.runtime.block_on(konnect.manager().stop_all());
                }
                self.active_routes.clear();
                self.install_menu();
            }
            Command::Quit => return true,
        }
        false
    }

    fn shutdown(&mut self) {
        if let Some(konnect) = self.konnect.take() {
            self.runtime.block_on(konnect.shutdown());
        }
        self.status_item.take();
    }

    fn update_transfer_stats(&mut self) {
        if let Some(status_item) = &mut self.status_item {
            status_item.set_menu_item_title(
                TRANSFER_STATS_ITEM_INDEX,
                transfer_label(&self.transfer_stats),
            );
        }
    }
}

pub fn run(config: Config) -> Result<()> {
    let contexts = configured_contexts(&config, kube_contexts()?);
    let forwards = config.forwards_for_contexts(&contexts)?;
    let runtime = Runtime::new().context("failed to create async runtime")?;
    let konnect = runtime.block_on(start_runtime(&forwards, config.proxy.port))?;
    let transfer_stats = konnect.transfer_stats();
    let (commands, receiver) = mpsc::channel();
    start_transfer_stats_updates(commands.clone());
    let app = Rc::new(RefCell::new(TrayApp {
        config,
        contexts,
        runtime,
        konnect: Some(konnect),
        status_item: None,
        routes: HashMap::new(),
        active_routes: HashSet::new(),
        transfer_stats,
        commands,
    }));
    app.borrow_mut().install_menu();

    let event_app = Rc::clone(&app);
    let terminator_slot: Rc<RefCell<Option<LoopTerminator>>> = Rc::new(RefCell::new(None));
    let event_terminator = Rc::clone(&terminator_slot);
    let (event_loop, terminator) = sync_event_loop(receiver, move |command| {
        if event_app.borrow_mut().handle(command)
            && let Some(terminator) = event_terminator.borrow().as_ref()
        {
            terminator.terminate();
        }
    });
    *terminator_slot.borrow_mut() = Some(terminator);
    event_loop();
    app.borrow_mut().shutdown();
    Ok(())
}

fn separator() -> MenuItem {
    MenuItem::separator()
}

fn service_name(route: &str) -> &str {
    route.rsplit_once('.').map_or(route, |(_, service)| service)
}

fn service_label(route: &str, active: bool) -> String {
    let name = service_name(route);
    if active {
        format!("{name} (active)")
    } else {
        name.to_owned()
    }
}

fn configured_contexts(config: &Config, contexts: Vec<String>) -> Vec<String> {
    let mut contexts: Vec<_> = contexts
        .into_iter()
        .filter(|context| config.configured_context(context) && !config.ignored_context(context))
        .collect();
    contexts.sort_by_cached_key(|context| {
        config
            .cluster_name_for_context(context)
            .expect("configured context must have a cluster name")
            .to_owned()
    });
    contexts
}

fn transfer_label(transfer_stats: &TransferStats) -> String {
    let (sent, received) = transfer_stats.snapshot();
    format!(
        "Sent: {:.2} MB | Received: {:.2} MB",
        sent as f64 / 1_000_000.0,
        received as f64 / 1_000_000.0
    )
}

fn start_transfer_stats_updates(commands: Sender<Command>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(1));
            if commands.send(Command::UpdateTransferStats).is_err() {
                return;
            }
        }
    });
}

fn context_label(config: &Config, context: &str) -> String {
    let name = config
        .cluster_name_for_context(context)
        .expect("configured context must have a cluster name");
    format!("{name} ({context})")
}

fn active_url(route: &str, proxy_port: u16) -> String {
    format!("http://{}:{proxy_port}", browser_host(route))
}

fn copy_to_clipboard(value: &str) -> Result<()> {
    let mut process = ProcessCommand::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to start pbcopy")?;
    process
        .stdin
        .take()
        .context("pbcopy did not accept standard input")?
        .write_all(value.as_bytes())
        .context("failed to write URL to pbcopy")?;
    let status = process.wait().context("failed waiting for pbcopy")?;
    if !status.success() {
        anyhow::bail!("pbcopy exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use konnect::{Config, TransferStats};

    use super::{
        STATUS_ICON, active_url, context_label, service_label, service_name, transfer_label,
    };

    #[test]
    fn embeds_the_kubernetes_status_icon() {
        assert!(STATUS_ICON.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn uses_service_segment_in_menu_labels() {
        assert_eq!(service_name("staging.grafana"), "grafana");
    }

    #[test]
    fn marks_active_services_without_checkbox_glyphs() {
        assert_eq!(service_label("usa.grafana", false), "grafana");
        assert_eq!(service_label("usa.grafana", true), "grafana (active)");
    }

    #[test]
    fn shows_the_cluster_alias_before_the_kube_context() {
        let config: Config = serde_json::from_str(
            r#"{"clusters":{"usa":{"context":"arn:aws:eks:us-west-2:123:cluster/example"}}}"#,
        )
        .unwrap();
        assert_eq!(
            context_label(&config, "arn:aws:eks:us-west-2:123:cluster/example"),
            "usa (arn:aws:eks:us-west-2:123:cluster/example)"
        );
    }

    #[test]
    fn formats_transfer_totals_in_megabytes() {
        let stats = TransferStats::default();
        assert_eq!(transfer_label(&stats), "Sent: 0.00 MB | Received: 0.00 MB");
    }

    #[test]
    fn builds_the_active_forward_url() {
        assert_eq!(
            active_url("usa.clickstack-app", 1355),
            "http://usa.clickstack-app.localhost:1355"
        );
    }
}
