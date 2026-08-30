//! Dynamic protobuf schema loading and gRPC service discovery.
//!
//! The first gRPC slice deliberately works from local `.proto` files. It keeps
//! descriptors in memory so the CLI and future GUI can share service, method
//! and JSON message handling without generating source code or invoking a
//! system `protoc` binary.

use std::path::{Path, PathBuf};

use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, MethodDescriptor};
use serde::Serialize;
use thiserror::Error;

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
}
