use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use prost::{DecodeError, Message};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorProto,
    FileDescriptorSet,
};
use tonic::Status;

/// v1 interface for the gRPC Reflection Service server.
pub mod v1;
/// Deprecated; access these via `v1` instead.
pub use v1::{ServerReflection, ServerReflectionServer}; // For backwards compatibility
/// v1alpha interface for the gRPC Reflection Service server.
pub mod v1alpha;

/// A builder used to construct a gRPC Reflection Service.
#[derive(Debug)]
pub struct Builder<'b> {
    file_descriptor_sets: Vec<FileDescriptorSet>,
    encoded_file_descriptor_sets: Vec<&'b [u8]>,
    include_reflection_service: bool,

    service_names: Vec<String>,
    use_all_service_names: bool,
}

impl<'b> Builder<'b> {
    /// Create a new builder that can configure a gRPC Reflection Service.
    pub fn configure() -> Self {
        Builder {
            file_descriptor_sets: Vec::new(),
            encoded_file_descriptor_sets: Vec::new(),
            include_reflection_service: true,

            service_names: Vec::new(),
            use_all_service_names: true,
        }
    }

    /// Registers an instance of `prost_types::FileDescriptorSet` with the gRPC Reflection
    /// Service builder.
    pub fn register_file_descriptor_set(mut self, file_descriptor_set: FileDescriptorSet) -> Self {
        self.file_descriptor_sets.push(file_descriptor_set);
        self
    }

    /// Registers a byte slice containing an encoded `prost_types::FileDescriptorSet` with
    /// the gRPC Reflection Service builder.
    pub fn register_encoded_file_descriptor_set(
        mut self,
        encoded_file_descriptor_set: &'b [u8],
    ) -> Self {
        self.encoded_file_descriptor_sets
            .push(encoded_file_descriptor_set);
        self
    }

    /// Serve the gRPC Reflection Service descriptor via the Reflection Service. This is enabled
    /// by default - set `include` to false to disable.
    pub fn include_reflection_service(mut self, include: bool) -> Self {
        self.include_reflection_service = include;
        self
    }

    /// Advertise a fully-qualified gRPC service name.
    ///
    /// If not called, then all services present in the registered file descriptor sets
    /// will be advertised.
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.use_all_service_names = false;
        self.service_names.push(name.into());
        self
    }

    /// Build a v1 gRPC Reflection Service to be served via Tonic.
    pub fn build(mut self) -> Result<v1::ServerReflectionServer<impl v1::ServerReflection>, Error> {
        if self.include_reflection_service {
            self = self.register_encoded_file_descriptor_set(crate::pb::v1::FILE_DESCRIPTOR_SET);
        }

        Ok(v1::ServerReflectionServer::new(
            v1::ReflectionService::from(ReflectionServiceState::new(
                self.service_names,
                self.encoded_file_descriptor_sets,
                self.file_descriptor_sets,
                self.use_all_service_names,
            )?),
        ))
    }

    /// Build a v1alpha gRPC Reflection Service to be served via Tonic.
    pub fn build_v1alpha(
        mut self,
    ) -> Result<v1alpha::ServerReflectionServer<impl v1alpha::ServerReflection>, Error> {
        if self.include_reflection_service {
            self =
                self.register_encoded_file_descriptor_set(crate::pb::v1alpha::FILE_DESCRIPTOR_SET);
        }

        Ok(v1alpha::ServerReflectionServer::new(
            v1alpha::ReflectionService::from(ReflectionServiceState::new(
                self.service_names,
                self.encoded_file_descriptor_sets,
                self.file_descriptor_sets,
                self.use_all_service_names,
            )?),
        ))
    }
}

#[derive(Debug)]
struct ReflectionServiceState {
    service_names: Vec<String>,
    files: HashMap<String, Arc<FileDescriptorProto>>,
    symbols: HashMap<String, Arc<FileDescriptorProto>>,
}

impl ReflectionServiceState {
    fn new(
        service_names: Vec<String>,
        encoded_file_descriptor_sets: Vec<&[u8]>,
        mut file_descriptor_sets: Vec<FileDescriptorSet>,
        use_all_service_names: bool,
    ) -> Result<Self, Error> {
        for encoded in encoded_file_descriptor_sets {
            file_descriptor_sets.push(FileDescriptorSet::decode(encoded)?);
        }

        let mut state = ReflectionServiceState {
            service_names,
            files: HashMap::new(),
            symbols: HashMap::new(),
        };

        for fds in file_descriptor_sets {
            for fd in fds.file {
                let name = match fd.name.clone() {
                    None => {
                        return Err(Error::InvalidFileDescriptorSet("missing name".to_string()));
                    }
                    Some(n) => n,
                };

                if state.files.contains_key(&name) {
                    continue;
                }

                let fd = Arc::new(fd);
                state.files.insert(name, fd.clone());
                state.process_file(fd, use_all_service_names)?;
            }
        }

        Ok(state)
    }

    fn process_file(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        use_all_service_names: bool,
    ) -> Result<(), Error> {
        let prefix = &fd.package.clone().unwrap_or_default();

        for msg in &fd.message_type {
            self.process_message(fd.clone(), prefix, msg)?;
        }

        for en in &fd.enum_type {
            self.process_enum(fd.clone(), prefix, en)?;
        }

        for service in &fd.service {
            let service_name = extract_name(prefix, "service", service.name.as_ref())?;
            if use_all_service_names {
                self.service_names.push(service_name.clone());
            }
            self.symbols.insert(service_name.clone(), fd.clone());

            for method in &service.method {
                let method_name = extract_name(&service_name, "method", method.name.as_ref())?;
                self.symbols.insert(method_name, fd.clone());
            }
        }

        Ok(())
    }

    fn process_message(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        msg: &DescriptorProto,
    ) -> Result<(), Error> {
        let message_name = extract_name(prefix, "message", msg.name.as_ref())?;
        self.symbols.insert(message_name.clone(), fd.clone());

        for nested in &msg.nested_type {
            self.process_message(fd.clone(), &message_name, nested)?;
        }

        for en in &msg.enum_type {
            self.process_enum(fd.clone(), &message_name, en)?;
        }

        for field in &msg.field {
            self.process_field(fd.clone(), &message_name, field)?;
        }

        for oneof in &msg.oneof_decl {
            let oneof_name = extract_name(&message_name, "oneof", oneof.name.as_ref())?;
            self.symbols.insert(oneof_name, fd.clone());
        }

        Ok(())
    }

    fn process_enum(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        en: &EnumDescriptorProto,
    ) -> Result<(), Error> {
        let enum_name = extract_name(prefix, "enum", en.name.as_ref())?;
        self.symbols.insert(enum_name.clone(), fd.clone());

        for value in &en.value {
            let value_name = extract_name(&enum_name, "enum value", value.name.as_ref())?;
            self.symbols.insert(value_name, fd.clone());
        }

        Ok(())
    }

    fn process_field(
        &mut self,
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        field: &FieldDescriptorProto,
    ) -> Result<(), Error> {
        let field_name = extract_name(prefix, "field", field.name.as_ref())?;
        self.symbols.insert(field_name, fd);
        Ok(())
    }

    fn list_services(&self) -> &[String] {
        &self.service_names
    }

    fn symbol_by_name(&self, symbol: &str) -> Result<Vec<u8>, Status> {
        match self.symbols.get(symbol) {
            None => Err(Status::not_found(format!("symbol '{}' not found", symbol))),
            Some(fd) => {
                let mut encoded_fd = Vec::new();
                if fd.clone().encode(&mut encoded_fd).is_err() {
                    return Err(Status::internal("encoding error"));
                };

                Ok(encoded_fd)
            }
        }
    }

    fn file_by_filename(&self, filename: &str) -> Result<Vec<u8>, Status> {
        match self.files.get(filename) {
            None => Err(Status::not_found(format!("file '{}' not found", filename))),
            Some(fd) => {
                let mut encoded_fd = Vec::new();
                if fd.clone().encode(&mut encoded_fd).is_err() {
                    return Err(Status::internal("encoding error"));
                }

                Ok(encoded_fd)
            }
        }
    }
}

fn extract_name(
    prefix: &str,
    name_type: &str,
    maybe_name: Option<&String>,
) -> Result<String, Error> {
    match maybe_name {
        None => Err(Error::InvalidFileDescriptorSet(format!(
            "missing {} name",
            name_type
        ))),
        Some(name) => {
            if prefix.is_empty() {
                Ok(name.to_string())
            } else {
                Ok(format!("{}.{}", prefix, name))
            }
        }
    }
}

/// Represents an error in the construction of a gRPC Reflection Service.
#[derive(Debug)]
pub enum Error {
    /// An error was encountered decoding a `prost_types::FileDescriptorSet` from a buffer.
    DecodeError(prost::DecodeError),
    /// An invalid `prost_types::FileDescriptorProto` was encountered.
    InvalidFileDescriptorSet(String),
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Error::DecodeError(e)
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DecodeError(_) => f.write_str("error decoding FileDescriptorSet from buffer"),
            Error::InvalidFileDescriptorSet(s) => {
                write!(f, "invalid FileDescriptorSet - {}", s)
            }
        }
    }
}
