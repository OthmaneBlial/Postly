//! Dynamic protobuf schema loading and gRPC service discovery.
//!
//! Descriptor handling stays in memory so the CLI and native GUI can share
//! service, method and JSON message handling for local `.proto` files and
//! servers exposing the standard reflection protocol.

use std::path::{Path, PathBuf};

use futures_util::stream;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, MethodDescriptor};
use serde::Serialize;
use thiserror::Error;
use tonic_reflection::pb::{v1, v1alpha};

/// Errors raised while loading a protobuf schema or converting a dynamic message.
#[derive(Debug, Error)]
pub enum GrpcError {
    /// The protobuf compiler could not parse or resolve the input files.
    #[error("failed to compile protobuf schema: {0}")]
    Compile(#[from] protox::Error),
    /// The descriptor pool could not be built from the compiled schema.
    #[error("failed to build protobuf descriptors: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    /// The supplied protobuf JSON message is invalid for its descriptor.
    #[error("invalid protobuf JSON message: {0}")]
    Json(#[from] serde_json::Error),
    /// The reflection service returned a transport-level error.
    #[error("gRPC reflection request failed: {0}")]
    ReflectionStatus(#[source] Box<tonic::Status>),
    /// The reflection service returned an incomplete or unsupported response.
    #[error("invalid gRPC reflection response: {0}")]
    ReflectionResponse(String),
    /// A descriptor returned by reflection could not be decoded.
    #[error("invalid reflected protobuf descriptor: {0}")]
    ReflectedDescriptor(String),
}

impl From<tonic::Status> for GrpcError {
    fn from(error: tonic::Status) -> Self {
        Self::ReflectionStatus(Box::new(error))
    }
}

/// A discovered gRPC service and its methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrpcServiceDescription {
    /// The short service name.
    pub name: String,
    /// The fully-qualified service name.
    pub full_name: String,
    /// Methods declared by this service.
    pub methods: Vec<GrpcMethodDescription>,
}

/// A discovered gRPC method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrpcMethodDescription {
    /// The short method name.
    pub name: String,
    /// The fully-qualified protobuf method name.
    pub full_name: String,
    /// The canonical gRPC path, such as `/demo.Echo/Echo`.
    pub path: String,
    /// The fully-qualified input message name.
    pub input: String,
    /// The fully-qualified output message name.
    pub output: String,
    /// Whether the method accepts a stream of client messages.
    pub client_streaming: bool,
    /// Whether the method returns a stream of server messages.
    pub server_streaming: bool,
}

/// A local protobuf descriptor pool with discovered gRPC services.
#[derive(Debug, Clone)]
pub struct GrpcSchema {
    pool: DescriptorPool,
    source: PathBuf,
}

impl GrpcSchema {
    /// Compile a root `.proto` file and its imports using local include paths.
    ///
    /// The root file's parent directory is always included, which makes a
    /// standalone proto file with sibling imports work without extra flags.
    pub fn from_proto(path: impl AsRef<Path>, includes: &[PathBuf]) -> Result<Self, GrpcError> {
        let path = path.as_ref();
        let mut include_roots = includes.to_vec();
        if let Some(parent) = path.parent() {
            if !include_roots.iter().any(|include| include == parent) {
                include_roots.push(parent.to_path_buf());
            }
        }
        let descriptors = protox::compile([path], include_roots)?;
        let pool = DescriptorPool::from_file_descriptor_set(descriptors)?;
        Ok(Self {
            pool,
            source: path.to_path_buf(),
        })
    }

    /// Build a schema from the serialized FileDescriptorProto values returned
    /// by the gRPC reflection service.
    pub fn from_descriptor_protos(
        descriptors: impl IntoIterator<Item = Vec<u8>>,
        source: impl Into<PathBuf>,
    ) -> Result<Self, GrpcError> {
        use prost_reflect::prost::Message;

        let files = descriptors
            .into_iter()
            .map(|bytes| {
                prost_reflect::prost_types::FileDescriptorProto::decode(bytes.as_slice())
                    .map_err(|error| GrpcError::ReflectedDescriptor(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if files.is_empty() {
            return Err(GrpcError::ReflectionResponse(
                "server returned no file descriptors".to_owned(),
            ));
        }
        let pool = DescriptorPool::from_file_descriptor_set(
            prost_reflect::prost_types::FileDescriptorSet { file: files },
        )?;
        Ok(Self {
            pool,
            source: source.into(),
        })
    }

    /// Discover services and message descriptors through gRPC reflection.
    ///
    /// The v1 protocol is attempted first, with a v1alpha fallback for older
    /// servers. The supplied channel is cloned internally so the caller can
    /// reuse it for the eventual dynamic method call.
    pub async fn from_reflection(
        channel: tonic::transport::Channel,
        host: impl Into<String>,
    ) -> Result<Self, GrpcError> {
        let host = host.into();
        match reflect_v1(channel.clone(), &host).await {
            Ok(schema) => Ok(schema),
            Err(v1_error) => reflect_v1alpha(channel, &host)
                .await
                .map_err(|v1alpha_error| {
                    GrpcError::ReflectionResponse(format!(
                        "v1 failed: {v1_error}; v1alpha failed: {v1alpha_error}"
                    ))
                }),
        }
    }

    /// Return the source `.proto` path used to build this schema.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Return descriptor file names, including imported files.
    pub fn files(&self) -> Vec<String> {
        self.pool
            .file_descriptor_protos()
            .filter_map(|file| file.name.clone())
            .collect()
    }

    /// Return all gRPC services in deterministic descriptor order.
    pub fn services(&self) -> Vec<GrpcServiceDescription> {
        self.pool
            .services()
            .map(|service| GrpcServiceDescription {
                name: service.name().to_owned(),
                full_name: service.full_name().to_owned(),
                methods: service
                    .methods()
                    .map(|method| GrpcMethodDescription {
                        name: method.name().to_owned(),
                        full_name: method.full_name().to_owned(),
                        path: format!("/{}/{}", service.full_name(), method.name()),
                        input: method.input().full_name().to_owned(),
                        output: method.output().full_name().to_owned(),
                        client_streaming: method.is_client_streaming(),
                        server_streaming: method.is_server_streaming(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// Find a method by gRPC path, fully-qualified protobuf name, or short service path.
    pub fn find_method(&self, requested: &str) -> Option<MethodDescriptor> {
        let requested = requested.trim_start_matches('/');
        self.pool.services().find_map(|service| {
            service.methods().find(|method| {
                let grpc_path = format!("{}/{}", service.full_name(), method.name());
                let short_path = format!("{}/{}", service.name(), method.name());
                requested == grpc_path || requested == short_path || requested == method.full_name()
            })
        })
    }
}

async fn reflect_v1(
    channel: tonic::transport::Channel,
    host: &str,
) -> Result<GrpcSchema, GrpcError> {
    let mut client = v1::server_reflection_client::ServerReflectionClient::new(channel);
    let services = match reflection_v1_response(
        &mut client,
        v1::ServerReflectionRequest {
            host: host.to_owned(),
            message_request: Some(v1::server_reflection_request::MessageRequest::ListServices(
                String::new(),
            )),
        },
    )
    .await?
    .message_response
    {
        Some(v1::server_reflection_response::MessageResponse::ListServicesResponse(services)) => {
            services
                .service
                .into_iter()
                .map(|service| service.name)
                .collect::<Vec<_>>()
        }
        Some(v1::server_reflection_response::MessageResponse::ErrorResponse(error)) => {
            return Err(GrpcError::ReflectionResponse(format!(
                "server error {}: {}",
                error.error_code, error.error_message
            )))
        }
        Some(other) => {
            return Err(GrpcError::ReflectionResponse(format!(
                "expected ListServicesResponse, received {other:?}"
            )))
        }
        None => {
            return Err(GrpcError::ReflectionResponse(
                "server returned no message response for ListServices".to_owned(),
            ))
        }
    };
    if services.is_empty() {
        return Err(GrpcError::ReflectionResponse(
            "server returned no gRPC services".to_owned(),
        ));
    }

    let mut descriptors = Vec::new();
    for service in services {
        let response = reflection_v1_response(
            &mut client,
            v1::ServerReflectionRequest {
                host: host.to_owned(),
                message_request: Some(
                    v1::server_reflection_request::MessageRequest::FileContainingSymbol(service),
                ),
            },
        )
        .await?;
        match response.message_response {
            Some(v1::server_reflection_response::MessageResponse::FileDescriptorResponse(
                response,
            )) => descriptors.extend(response.file_descriptor_proto),
            Some(v1::server_reflection_response::MessageResponse::ErrorResponse(error)) => {
                return Err(GrpcError::ReflectionResponse(format!(
                    "server error {}: {}",
                    error.error_code, error.error_message
                )))
            }
            Some(other) => {
                return Err(GrpcError::ReflectionResponse(format!(
                    "expected FileDescriptorResponse, received {other:?}"
                )))
            }
            None => {
                return Err(GrpcError::ReflectionResponse(
                    "server returned no descriptor response".to_owned(),
                ))
            }
        }
    }
    GrpcSchema::from_descriptor_protos(
        descriptors,
        format!(
            "reflection://{}",
            if host.is_empty() { "server" } else { host }
        ),
    )
}

async fn reflection_v1_response(
    client: &mut v1::server_reflection_client::ServerReflectionClient<tonic::transport::Channel>,
    request: v1::ServerReflectionRequest,
) -> Result<v1::ServerReflectionResponse, GrpcError> {
    let response = client
        .server_reflection_info(stream::iter(vec![request]))
        .await?;
    let mut stream = response.into_inner();
    stream
        .message()
        .await?
        .ok_or_else(|| GrpcError::ReflectionResponse("reflection stream ended early".to_owned()))
}

async fn reflect_v1alpha(
    channel: tonic::transport::Channel,
    host: &str,
) -> Result<GrpcSchema, GrpcError> {
    let mut client = v1alpha::server_reflection_client::ServerReflectionClient::new(channel);
    let services = match reflection_v1alpha_response(
        &mut client,
        v1alpha::ServerReflectionRequest {
            host: host.to_owned(),
            message_request: Some(
                v1alpha::server_reflection_request::MessageRequest::ListServices(String::new()),
            ),
        },
    )
    .await?
    .message_response
    {
        Some(v1alpha::server_reflection_response::MessageResponse::ListServicesResponse(
            services,
        )) => services
            .service
            .into_iter()
            .map(|service| service.name)
            .collect::<Vec<_>>(),
        Some(v1alpha::server_reflection_response::MessageResponse::ErrorResponse(error)) => {
            return Err(GrpcError::ReflectionResponse(format!(
                "server error {}: {}",
                error.error_code, error.error_message
            )))
        }
        Some(other) => {
            return Err(GrpcError::ReflectionResponse(format!(
                "expected ListServicesResponse, received {other:?}"
            )))
        }
        None => {
            return Err(GrpcError::ReflectionResponse(
                "server returned no message response for ListServices".to_owned(),
            ))
        }
    };
    if services.is_empty() {
        return Err(GrpcError::ReflectionResponse(
            "server returned no gRPC services".to_owned(),
        ));
    }

    let mut descriptors = Vec::new();
    for service in services {
        let response = reflection_v1alpha_response(
            &mut client,
            v1alpha::ServerReflectionRequest {
                host: host.to_owned(),
                message_request: Some(
                    v1alpha::server_reflection_request::MessageRequest::FileContainingSymbol(
                        service,
                    ),
                ),
            },
        )
        .await?;
        match response.message_response {
            Some(v1alpha::server_reflection_response::MessageResponse::FileDescriptorResponse(
                response,
            )) => descriptors.extend(response.file_descriptor_proto),
            Some(v1alpha::server_reflection_response::MessageResponse::ErrorResponse(error)) => {
                return Err(GrpcError::ReflectionResponse(format!(
                    "server error {}: {}",
                    error.error_code, error.error_message
                )))
            }
            Some(other) => {
                return Err(GrpcError::ReflectionResponse(format!(
                    "expected FileDescriptorResponse, received {other:?}"
                )))
            }
            None => {
                return Err(GrpcError::ReflectionResponse(
                    "server returned no descriptor response".to_owned(),
                ))
            }
        }
    }
    GrpcSchema::from_descriptor_protos(
        descriptors,
        format!(
            "reflection://{}",
            if host.is_empty() { "server" } else { host }
        ),
    )
}

async fn reflection_v1alpha_response(
    client: &mut v1alpha::server_reflection_client::ServerReflectionClient<
        tonic::transport::Channel,
    >,
    request: v1alpha::ServerReflectionRequest,
) -> Result<v1alpha::ServerReflectionResponse, GrpcError> {
    let response = client
        .server_reflection_info(stream::iter(vec![request]))
        .await?;
    let mut stream = response.into_inner();
    stream
        .message()
        .await?
        .ok_or_else(|| GrpcError::ReflectionResponse("reflection stream ended early".to_owned()))
}

/// Decode a protobuf JSON object into a dynamically typed message.
pub fn message_from_json(
    descriptor: MessageDescriptor,
    input: &str,
) -> Result<DynamicMessage, GrpcError> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let message = DynamicMessage::deserialize(descriptor, &mut deserializer)?;
    deserializer.end()?;
    Ok(message)
}

/// Convert a dynamically typed protobuf message to canonical protobuf JSON.
pub fn message_to_json(message: &DynamicMessage) -> Result<serde_json::Value, GrpcError> {
    Ok(serde_json::to_value(message)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_proto_and_discovers_unary_method() {
        let directory = tempfile::tempdir().expect("tempdir");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;

                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }

                service Echo {
                    rpc Echo(EchoRequest) returns (EchoResponse);
                }
            "#,
        )
        .expect("proto");

        let schema = GrpcSchema::from_proto(&proto, &[]).expect("schema");
        let services = schema.services();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].full_name, "demo.Echo");
        assert_eq!(services[0].methods[0].path, "/demo.Echo/Echo");
        assert!(!services[0].methods[0].client_streaming);
        assert!(!services[0].methods[0].server_streaming);

        let method = schema.find_method("/demo.Echo/Echo").expect("method");
        let request = message_from_json(method.input(), r#"{"message":"hello"}"#).expect("request");
        assert_eq!(
            message_to_json(&request).expect("json"),
            serde_json::json!({"message": "hello"})
        );
    }

    #[tokio::test]
    async fn discovers_services_through_v1_reflection() {
        use prost_reflect::prost::Message;
        use tokio_stream::wrappers::TcpListenerStream;

        let directory = tempfile::tempdir().expect("tempdir");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;

                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }

                service Echo {
                    rpc Echo(EchoRequest) returns (EchoResponse);
                }
            "#,
        )
        .expect("proto");
        let descriptors = protox::compile([&proto], vec![directory.path().to_path_buf()])
            .expect("descriptor set");
        let mut encoded = Vec::new();
        descriptors
            .encode(&mut encoded)
            .expect("encode descriptors");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let reflection = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(&encoded)
                .build_v1()
                .expect("reflection service");
            tonic::transport::Server::builder()
                .add_service(reflection)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = shutdown_receiver.await;
                })
                .await
                .expect("server");
        });

        let channel = tonic::transport::Endpoint::from_shared(endpoint)
            .expect("endpoint")
            .connect()
            .await
            .expect("channel");
        let schema = GrpcSchema::from_reflection(channel, "")
            .await
            .expect("reflected schema");
        let method = schema.find_method("/demo.Echo/Echo").expect("method");
        assert_eq!(method.input().full_name(), "demo.EchoRequest");
        assert_eq!(method.output().full_name(), "demo.EchoResponse");
        assert!(schema.files().iter().any(|file| file == "echo.proto"));

        shutdown_sender.send(()).expect("shutdown");
        server.await.expect("server task");
    }
}
