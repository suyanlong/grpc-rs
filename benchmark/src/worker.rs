// Copyright 2017 PingCAP, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.


use std::sync::{Arc, Mutex};

use grpc_proto::testing::services_grpc::WorkerService;
use grpc_proto::testing::control::{ClientArgs, ClientStatus, CoreRequest, CoreResponse,
                                   ServerArgs, ServerStatus, Void};
use grpc::{DuplexSink, RequestStream, RpcContext, UnarySink, WriteFlags};
use error::Error;
use futures::{future, Future, Sink, Stream};
use futures::sync::oneshot::Sender;

use client::Client;
use util;
use server::Server;

#[derive(Clone)]
pub struct Worker {
    shutdown_notifier: Arc<Mutex<Option<Sender<()>>>>,
}

impl Worker {
    pub fn new(sender: Sender<()>) -> Worker {
        Worker {
            shutdown_notifier: Arc::new(Mutex::new(Some(sender))),
        }
    }
}

impl WorkerService for Worker {
    fn run_server(
        &self,
        ctx: RpcContext,
        stream: RequestStream<ServerArgs>,
        sink: DuplexSink<ServerStatus>,
    ) {
        let f = stream
            .into_future()
            .map_err(|(e, _)| Error::from(e))
            .and_then(|(arg, stream)| {
                let cfg = arg.as_ref().unwrap().get_setup();
                println!("receive server setup: {:?}", cfg);
                let server = try!(Server::new(cfg));
                let status = server.get_status();
                Ok(
                    sink.send((status, WriteFlags::default()))
                        .and_then(|sink| {
                            stream.fold((sink, server), |(sink, mut server), arg| {
                                let mark = arg.get_mark();
                                println!("receive server mark: {:?}", mark);
                                let stats = server.get_stats(mark.get_reset());
                                let mut status = server.get_status();
                                status.set_stats(stats);
                                sink.send((status, WriteFlags::default()))
                                    .map(|sink| (sink, server))
                            })
                        })
                        .and_then(|(sink, mut server)| server.shutdown().map(|_| sink))
                        .and_then(|mut sink| future::poll_fn(move || sink.close()))
                        .map_err(Error::from),
                )
            })
            .flatten()
            .map_err(|e| println!("run server failed: {:?}", e))
            .map(|_| println!("server shutdown."));
        ctx.spawn(f)
    }

    fn run_client(
        &self,
        ctx: RpcContext,
        stream: RequestStream<ClientArgs>,
        sink: DuplexSink<ClientStatus>,
    ) {
        let f = stream
            .into_future()
            .map_err(|(e, _)| Error::from(e))
            .and_then(|(arg, stream)| {
                let cfg = arg.as_ref().unwrap().get_setup();
                println!("receive client setup: {:?}", cfg);
                let client = Client::new(cfg);
                sink.send((ClientStatus::new(), WriteFlags::default()))
                    .and_then(|sink| {
                        stream.fold((sink, client), |(sink, mut client), arg| {
                            let mark = arg.get_mark();
                            println!("receive client mark: {:?}", mark);
                            let stats = client.get_stats(mark.get_reset());
                            let mut status = ClientStatus::new();
                            status.set_stats(stats);
                            sink.send((status, WriteFlags::default()))
                                .map(|sink| (sink, client))
                        })
                    })
                    .map_err(Error::from)
                    .and_then(|(sink, mut client)| client.shutdown().map(|_| sink))
                    .and_then(|mut sink| {
                        future::poll_fn(move || sink.close().map_err(From::from))
                    })
            })
            .map_err(|e| println!("run client failed: {:?}", e))
            .map(|_| println!("client shutdown."));
        ctx.spawn(f)
    }

    fn core_count(&self, ctx: RpcContext, _: CoreRequest, sink: UnarySink<CoreResponse>) {
        let cpu_count = util::cpu_num_cores();
        let mut resp = CoreResponse::new();
        resp.set_cores(cpu_count as i32);
        ctx.spawn(
            sink.success(resp)
                .map_err(|e| println!("failed to report cpu count: {:?}", e)),
        )
    }

    fn quit_worker(&self, ctx: RpcContext, _: Void, sink: ::grpc::UnarySink<Void>) {
        let notifier = self.shutdown_notifier.lock().unwrap().take();
        if let Some(notifier) = notifier {
            let _ = notifier.send(());
        }
        ctx.spawn(
            sink.success(Void::new())
                .map_err(|e| println!("failed to report quick worker: {:?}", e)),
        );
    }
}
