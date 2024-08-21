use core::str;
use std::{borrow::Borrow, cell::RefCell, future::{ready, Ready}, rc::Rc, time::Duration};
use actix_http::h1::Payload;
use actix_web::{body::{self, BoxBody}, dev::{Service, ServiceRequest, ServiceResponse, Transform}, http::header::HeaderMap, web::BytesMut, Error, HttpMessage, HttpResponse};
use futures_util::{future::LocalBoxFuture, StreamExt};
use opentelemetry::{
    global::{self, BoxedSpan}, propagation::Extractor, trace::{Span, SpanKind, Tracer}, KeyValue
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator, runtime::Tokio, trace::{self, Config, Sampler, TracerProvider}, Resource,
};
use opentelemetry_stdout::SpanExporterBuilder;
use actix_web::HttpResponseBuilder;
mod semantic_conventions;

pub fn init_tracer() {
    global::set_text_map_propagator(TraceContextPropagator::new());

   let provider = opentelemetry_otlp::new_pipeline()
         .tracing()
         .with_exporter(
             opentelemetry_otlp::new_exporter()
             .tonic()
             .with_endpoint("http://35.233.243.10:4317")
             .with_timeout(Duration::from_secs(3))
          )
         .with_trace_config(
            Config::default()
            .with_sampler(Sampler::AlwaysOn)
            .with_id_generator(trace::RandomIdGenerator::default())
            .with_max_events_per_span(64)
            .with_max_attributes_per_span(64)
            .with_max_events_per_span(64)
            .with_resource(Resource::new(vec![KeyValue::new("service.name", "jacob-test")])),
        )
         .install_batch(opentelemetry_sdk::runtime::Tokio);

    global::set_tracer_provider(provider.expect("could not initialise tracer provider"));
    // let provider = TracerProvider::builder()
    //     .with_batch_exporter(
    //         SpanExporterBuilder::default()
    //             .with_encoder(|writer, data| {
    //                 serde_json::to_writer_pretty(writer, &data).unwrap();
    //                 Ok(())
    //             })
    //             .build(),
    //         Tokio,
    //     )
    //     .build();

    // global::set_tracer_provider(provider);
}

pub struct RustAgentMiddleware<S> {
    service: Rc<RefCell<S>>,
}


impl<S> Service<ServiceRequest> for RustAgentMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error> + 'static,
    S::Future: 'static,
    // B: 'static + actix_web::body::BoxBody,
{
    type Response = ServiceResponse<BoxBody>;
    type Error  = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;
    
    // forward_ready!(service);
    
    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();
        // create span
        let method = req.method().to_string();
        let tracer = global::tracer("actix");
        let mut span = tracer
            .span_builder(method)
            .with_kind(SpanKind::Server)
            .start(&tracer);

        let uri = req.uri().clone();
        
        span.set_attribute(KeyValue{
            key: semantic_conventions::HTTP_URL.into(),
            value: req.request().full_url().to_string().into()
        });

        span.set_attribute(KeyValue{
            key: semantic_conventions::HTTP_TARGET.into(),
            value: String::from(uri.path_and_query().expect("path and query is empty").as_str()).into()
        });

        // span.set_attribute(KeyValue{
        //     key: semantic_conventions::HTTP_SCHEME.into(),
        //     value: String::from(uri.scheme_str().expect("scheme is empty")).into()
        // });

        populate_headers(&mut span, req.headers(), semantic_conventions::HTTP_REQUEST_HEADER_PREFIX, &mut None);

        Box::pin(async move {
            let mut request_body = BytesMut::new();
            while let Some(chunk) = req.take_payload().next().await {
                request_body.extend_from_slice(&chunk?);
            }
    
            let mut orig_payload = Payload::create(true);
            orig_payload.1.unread_data(request_body.clone().freeze());
            req.set_payload(actix_http::Payload::from(orig_payload.1));
            span.set_attribute(KeyValue{
                key: semantic_conventions::HTTP_REQUEST_BODY.into(),
                value: String::from_utf8(request_body.to_vec()).expect("couldn't extract body").into()
            });

            let mut res = svc.call(req).await?;
            let status = res.status().clone();

            populate_headers(&mut span, res.headers(), semantic_conventions::HTTP_RESPONSE_HEADER_PREFIX, &mut None);
            span.set_attribute(KeyValue {
                key: semantic_conventions::HTTP_STATUS_CODE.into(),
                value: String::from(res.status().as_str()).into(),
            });

            let content_type = match res.borrow().headers().get("content-type") {
                None => { "unknown"}
                Some(header) => {
                    match header.to_str() {
                        Ok(value) => {value}
                        Err(_) => { "unknown"}
                    }
                }
            };

            let ret = match content_type.to_ascii_lowercase().contains("json") {
                false => {res}
                true => {
                    let new_request = res.request().clone();
                    let headers = res.headers().clone();
                    let body_bytes = &body::to_bytes(res.into_body()).await?;
                    let body_data = match str::from_utf8(&body_bytes){
                        Ok(str) => {
                            str
                        }
                        Err(_) => {
                            "Unknown"
                        }
                    };

                    let mut new_response = 
                            HttpResponseBuilder::new(status)
                                .body(BoxBody::new(String::from(body_data)));
                    span.set_attribute(KeyValue {
                        key: semantic_conventions::HTTP_RESPONSE_BODY.into(),
                        value: String::from(body_data).into()
                    });

                    for (key, value) in headers.iter() {
                        new_response.headers_mut().insert(key.clone(), value.clone());
                    }

                    ServiceResponse::new(new_request, new_response)
                }
            };
                                
            span.end();
            Ok(ret)
        })
    }    
    
    fn poll_ready(&self, _ctx: &mut core::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

}

pub struct RustAgent;

// `B` - type of response's body
impl<S: 'static> Transform<S, ServiceRequest> for RustAgent
where
    S: Service<ServiceRequest, Response = ServiceResponse<BoxBody>, Error = Error>,
    S::Future: 'static,
    // B: 'static + actix_web::body::BoxBody,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = RustAgentMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RustAgentMiddleware { service: Rc::new(RefCell::new(service)) }))
    }
}

fn populate_headers(span: &mut BoxedSpan, headers: &HeaderMap, prefix: &str, clone_response: &mut Option<HttpResponse>) {
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

        span.set_attribute(KeyValue{
            key: attr_key.into(),
            value: attr_value.into(),
        });
    }

}
