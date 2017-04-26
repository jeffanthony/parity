// Copyright 2015-2017 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::io;
use std::sync::Arc;
use std::path::PathBuf;

use dapps;
use dir::default_data_path;
use helpers::{parity_ipc_path, replace_home};
use jsonrpc_core::MetaIoHandler;
use parity_reactor::TokioRemote;
use parity_rpc::informant::{RpcStats, Middleware};
use parity_rpc::{self as rpc, Metadata, DomainsValidation};
use rpc_apis::{self, ApiSet};

pub use parity_rpc::{IpcServer, HttpServer, RequestMiddleware};
pub use parity_rpc::ws::Server as WsServer;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpConfiguration {
	pub enabled: bool,
	pub interface: String,
	pub port: u16,
	pub apis: ApiSet,
	pub cors: Option<Vec<String>>,
	pub hosts: Option<Vec<String>>,
	pub threads: Option<usize>,
}

impl HttpConfiguration {
	pub fn address(&self) -> Option<(String, u16)> {
		match self.enabled {
			true => Some((self.interface.clone(), self.port)),
			false => None,
		}
	}
}

impl Default for HttpConfiguration {
	fn default() -> Self {
		HttpConfiguration {
			enabled: true,
			interface: "127.0.0.1".into(),
			port: 8545,
			apis: ApiSet::UnsafeContext,
			cors: None,
			hosts: Some(Vec::new()),
			threads: None,
		}
	}
}

#[derive(Debug, PartialEq, Clone)]
pub struct UiConfiguration {
	pub enabled: bool,
	pub interface: String,
	pub port: u16,
	pub hosts: Option<Vec<String>>,
}

impl UiConfiguration {
	pub fn address(&self) -> Option<(String, u16)> {
		match self.enabled {
			true => Some((self.interface.clone(), self.port)),
			false => None,
		}
	}
}

impl From<UiConfiguration> for HttpConfiguration {
	fn from(conf: UiConfiguration) -> Self {
		HttpConfiguration {
			enabled: conf.enabled,
			interface: conf.interface,
			port: conf.port,
			apis: rpc_apis::ApiSet::SafeContext,
			cors: None,
			hosts: conf.hosts,
			threads: None,
		}
	}
}

impl Default for UiConfiguration {
	fn default() -> Self {
		UiConfiguration {
			enabled: true,
			port: 8180,
			interface: "127.0.0.1".into(),
			hosts: Some(vec![]),
		}
	}
}

#[derive(Debug, PartialEq)]
pub struct IpcConfiguration {
	pub enabled: bool,
	pub socket_addr: String,
	pub apis: ApiSet,
}

impl Default for IpcConfiguration {
	fn default() -> Self {
		let data_dir = default_data_path();
		IpcConfiguration {
			enabled: true,
			socket_addr: parity_ipc_path(&data_dir, "$BASE/jsonrpc.ipc"),
			apis: ApiSet::IpcContext,
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct WsConfiguration {
	pub enabled: bool,
	pub interface: String,
	pub port: u16,
	pub apis: ApiSet,
	pub origins: Option<Vec<String>>,
	pub hosts: Option<Vec<String>>,
	pub signer_path: PathBuf,
}

impl Default for WsConfiguration {
	fn default() -> Self {
		let data_dir = default_data_path();
		WsConfiguration {
			enabled: true,
			interface: "127.0.0.1".into(),
			port: 8546,
			apis: ApiSet::UnsafeContext,
			origins: Some(Vec::new()),
			hosts: Some(Vec::new()),
			signer_path: replace_home(&data_dir, "$BASE/signer").into(),
		}
	}
}

impl WsConfiguration {
	pub fn address(&self) -> Option<(String, u16)> {
		match self.enabled {
			true => Some((self.interface.clone(), self.port)),
			false => None,
		}
	}
}

pub struct Dependencies<D: rpc_apis::Dependencies> {
	pub apis: Arc<D>,
	pub remote: TokioRemote,
	pub stats: Arc<RpcStats>,
}

pub fn new_ws<D: rpc_apis::Dependencies>(
	conf: WsConfiguration,
	deps: &Dependencies<D>,
) -> Result<Option<WsServer>, String> {
	if !conf.enabled {
		return Ok(None);
	}

	let url = format!("{}:{}", conf.interface, conf.port);
	let addr = url.parse().map_err(|_| format!("Invalid WebSockets listen host/port given: {}", url))?;


	let full_handler = setup_apis(rpc_apis::ApiSet::SafeContext, deps);
	let handler = {
		let mut handler = MetaIoHandler::with_middleware((
			rpc::WsDispatcher::new(full_handler),
			Middleware::new(deps.stats.clone(), deps.apis.activity_notifier())
		));
		let apis = conf.apis.list_apis().into_iter().collect::<Vec<_>>();
		deps.apis.extend_with_set(&mut handler, &apis);

		handler
	};

	let remote = deps.remote.clone();
	let allowed_origins = into_domains(conf.origins);
	let allowed_hosts = into_domains(conf.hosts);

	let path = ::signer::codes_path(&conf.signer_path);
	let start_result = rpc::start_ws(
		&addr,
		handler,
		remote,
		allowed_origins,
		allowed_hosts,
		// TODO [ToDr] Codes should be provided only if signer is enabled!
		rpc::WsExtractor::new(Some(&path)),
		rpc::WsExtractor::new(Some(&path)),
		rpc::WsStats::new(deps.stats.clone()),
	);

	match start_result {
		Ok(server) => Ok(Some(server)),
		Err(rpc::ws::Error::Io(ref err)) if err.kind() == io::ErrorKind::AddrInUse => Err(
			format!("WebSockets address {} is already in use, make sure that another instance of an Ethereum client is not running or change the address using the --ws-port and --ws-interface options.", url)
		),
		Err(e) => Err(format!("WebSockets error: {:?}", e)),
	}
}

pub fn new_http<D: rpc_apis::Dependencies>(
	id: &str,
	options: &str,
	conf: HttpConfiguration,
	deps: &Dependencies<D>,
	middleware: Option<dapps::Middleware>,
) -> Result<Option<HttpServer>, String> {
	if !conf.enabled {
		return Ok(None);
	}

	let url = format!("{}:{}", conf.interface, conf.port);
	let addr = url.parse().map_err(|_| format!("Invalid {} listen host/port given: {}", id, url))?;
	let handler = setup_apis(conf.apis, deps);
	let remote = deps.remote.clone();

	let cors_domains = into_domains(conf.cors);
	let allowed_hosts = into_domains(conf.hosts);

	let start_result = rpc::start_http(
		&addr,
		cors_domains,
		allowed_hosts,
		handler,
		remote,
		rpc::RpcExtractor,
		match (conf.threads, middleware) {
			(Some(threads), None) => rpc::HttpSettings::Threads(threads),
			(None, middleware) => rpc::HttpSettings::Dapps(middleware),
			(Some(_), Some(_)) => {
				return Err("Dapps and fast multi-threaded RPC server cannot be enabled at the same time.".into())
			},
		}
	);

	match start_result {
		Ok(server) => Ok(Some(server)),
		Err(rpc::HttpServerError::Io(ref err)) if err.kind() == io::ErrorKind::AddrInUse => Err(
			format!("{} address {} is already in use, make sure that another instance of an Ethereum client is not running or change the address using the --{}-port and --{}-interface options.", id, url, options, options)
		),
		Err(e) => Err(format!("{} error: {:?}", id, e)),
	}
}

pub fn new_ipc<D: rpc_apis::Dependencies>(
	conf: IpcConfiguration,
	dependencies: &Dependencies<D>
) -> Result<Option<IpcServer>, String> {
	if !conf.enabled {
		return Ok(None);
	}

	let handler = setup_apis(conf.apis, dependencies);
	let remote = dependencies.remote.clone();
	match rpc::start_ipc(&conf.socket_addr, handler, remote, rpc::RpcExtractor) {
		Ok(server) => Ok(Some(server)),
		Err(io_error) => Err(format!("IPC error: {}", io_error)),
	}
}

fn into_domains<T: From<String>>(items: Option<Vec<String>>) -> DomainsValidation<T> {
	items.map(|vals| vals.into_iter().map(T::from).collect()).into()
}

fn setup_apis<D>(apis: ApiSet, deps: &Dependencies<D>) -> MetaIoHandler<Metadata, Middleware<D::Notifier>>
	where D: rpc_apis::Dependencies
{
	let mut handler = MetaIoHandler::with_middleware(
		Middleware::new(deps.stats.clone(), deps.apis.activity_notifier())
	);
	let apis = apis.list_apis().into_iter().collect::<Vec<_>>();
	deps.apis.extend_with_set(&mut handler, &apis);

	handler
}
