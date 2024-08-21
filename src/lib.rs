use actix_http::h1::Payload;
use actix_web::HttpResponseBuilder;
use actix_web::{
    body::{self, BoxBody},
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    http::header::HeaderMap,
    web::BytesMut,
    Error, HttpMessage, HttpResponse,
};
use core::str;
use futures_util::{future::LocalBoxFuture, StreamExt};
use opentelemetry::{
    global::{self, BoxedSpan},
    propagation::Extractor,
    trace::{Span, SpanKind, Tracer},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime::Tokio,
    trace::{self, Config, Sampler, TracerProvider},
    Resource,
};
use opentelemetry_stdout::SpanExporterBuilder;
use std::{
    borrow::Borrow,
    cell::RefCell,
    future::{ready, Ready},
    rc::Rc,
    time::Duration,
};
mod config;
mod semantic_conventions;

pub fn init_tracer() {
    let cfg = config::load();
    println!("loading agent with config: {:?}", cfg);

    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter_cfg = cfg.exporter.clone().unwrap();
    let provider = match exporter_cfg.trace_reporter_type.unwrap() {
        config::TraceReporterType::Otlp => opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(exporter_cfg.endpoint.unwrap())
                    .with_timeout(Duration::from_secs(10)),
            )
            .with_trace_config(
                Config::default()
                    .with_sampler(Sampler::AlwaysOn)
                    .with_id_generator(trace::RandomIdGenerator::default())
                    .with_max_events_per_span(64)
                    .with_max_attributes_per_span(64)
                    .with_max_events_per_span(64)
                    .with_resource(Resource::new(vec![KeyValue::new(
                        "service.name",
                        cfg.service_name.unwrap(),
                    )])),
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .unwrap(),
        config::TraceReporterType::Logging => TracerProvider::builder()
            .with_batch_exporter(
                SpanExporterBuilder::default()
                    .with_encoder(|writer, data| {
                        serde_json::to_writer_pretty(writer, &data).unwrap();
                        Ok(())
                    })
                    .build(),
                Tokio,
            )
            .build(),
    };

    global::set_tracer_provider(provider);
}

pub struct RustAgentMiddleware<S> {
    service: Rc<RefCell<S>>,
    config: config::Config,
}

impl<S> Service<ServiceRequest> for RustAgentMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();
        // create span
        let method = req.method().to_string();
        let tracer = global::tracer("actix");
        let mut span = tracer
            .span_builder(method.clone())
            .with_kind(SpanKind::Server)
            .start(&tracer);

        let uri = req.uri().clone();

        span.set_attribute(KeyValue {
            key: semantic_conventions::HTTP_URL.into(),
            value: req.request().full_url().to_string().into(),
        });

        span.set_attribute(KeyValue {
            key: semantic_conventions::HTTP_TARGET.into(),
            value: String::from(
                uri.path_and_query()
                    .expect("path and query is empty")
                    .as_str(),
            )
            .into(),
        });

        span.set_attribute(KeyValue {
            key: semantic_conventions::HTTP_METHOD.into(),
            value: method.clone().into(),
        });

        match uri.scheme_str() {
            Some(scheme) => span.set_attribute(KeyValue {
                key: semantic_conventions::HTTP_SCHEME.into(),
                value: String::from(scheme).into(),
            }),
            None => {}
        }

        populate_headers(
            &mut span,
            req.headers(),
            semantic_conventions::HTTP_REQUEST_HEADER_PREFIX,
        );

        Box::pin(async move {
            let mut request_body = BytesMut::new();
            while let Some(chunk) = req.take_payload().next().await {
                request_body.extend_from_slice(&chunk?);
            }

            let mut orig_payload = Payload::create(true);
            orig_payload.1.unread_data(request_body.clone().freeze());
            req.set_payload(actix_http::Payload::from(orig_payload.1));
            span.set_attribute(KeyValue {
                key: semantic_conventions::HTTP_REQUEST_BODY.into(),
                value: String::from_utf8(request_body.to_vec())
                    .expect("couldn't extract body")
                    .into(),
            });

            let res = svc.call(req).await?;
            let status = res.status().clone();

            populate_headers(
                &mut span,
                res.headers(),
                semantic_conventions::HTTP_RESPONSE_HEADER_PREFIX,
            );
            span.set_attribute(KeyValue {
                key: semantic_conventions::HTTP_STATUS_CODE.into(),
                value: String::from(res.status().as_str()).into(),
            });

            let content_type = match res.borrow().headers().get("content-type") {
                None => "unknown",
                Some(header) => match header.to_str() {
                    Ok(value) => value,
                    Err(_) => "unknown",
                },
            };

            let ret = match true {
                false => res,
                true => {
                    let new_request = res.request().clone();
                    let headers = res.headers().clone();
                    let body_bytes = &body::to_bytes(res.into_body()).await?;
                    let body_data = match str::from_utf8(&body_bytes) {
                        Ok(str) => str,
                        Err(_) => "Unknown",
                    };

                    let mut new_response = HttpResponseBuilder::new(status)
                        .body(BoxBody::new(String::from(body_data)));
                    span.set_attribute(KeyValue {
                        key: semantic_conventions::HTTP_RESPONSE_BODY.into(),
                        value: String::from(body_data).into(),
                    });

                    for (key, value) in headers.iter() {
                        new_response
                            .headers_mut()
                            .insert(key.clone(), value.clone());
                    }

                    ServiceResponse::new(new_request, new_response)
                }
            };

            span.end();
            Ok(ret)
        })
    }

    fn poll_ready(
        &self,
        _ctx: &mut core::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

// impl<S> RustAgentMiddleware<S> {
//     fn should_capture_content_type(self: &Self, content_type: &str) -> bool {

//         for allowed_type in self.config.allowed_content_types.clone().unwrap().iter() {
//             if content_type.to_ascii_lowercase().contains(allowed_type) {
//                 return true;
//             }
//         }
//         false
//     }
// }

pub struct RustAgent {
    pub config: config::Config,
}

impl<S: 'static> Transform<S, ServiceRequest> for RustAgent
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error>,
    S::Future: 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = RustAgentMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RustAgentMiddleware {
            service: Rc::new(RefCell::new(service)),
            config: self.config.clone(),
        }))
    }
}

impl Default for RustAgent {
    fn default() -> Self {
        Self {
            config: config::load(),
        }
    }
}

fn populate_headers(span: &mut BoxedSpan, headers: &HeaderMap, prefix: &str) {
    let mut itr = headers.iter();

    loop {
        let header = itr.next();
        if header == None {
            break;
        }

        let val = header.expect("header is empty");
        let mut attr_key = "".to_owned();
        attr_key.push_str(prefix);
        attr_key.push_str(val.0.as_str());
        let mut attr_value = "".to_owned();
        attr_value.push_str(val.1.to_str().expect("header value is not string"));

        span.set_attribute(KeyValue {
            key: attr_key.into(),
            value: attr_value.into(),
        });
    }
}
