use crate::server::Error;
use prost::Message;
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorProto,
    FileDescriptorSet,
};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct DescriptorInfo {
    pub(crate) service_names: Vec<String>,
    pub(crate) symbols: HashMap<String, Arc<FileDescriptorProto>>,
    pub(crate) files: HashMap<String, Arc<FileDescriptorProto>>,
}

type SymbolArray = Vec<(String, Arc<FileDescriptorProto>)>;

struct File {
    service_names: Vec<String>,
    symbols: SymbolArray,
}

pub(crate) struct DescriptorParser {}

impl DescriptorParser {
    pub(crate) fn process(
        encoded_file_descriptor_sets: Vec<&[u8]>,
        file_descriptor_sets: Vec<FileDescriptorSet>,
    ) -> Result<DescriptorInfo, Error> {
        let mut all_fds = file_descriptor_sets.clone();

        for encoded in &encoded_file_descriptor_sets {
            let decoded = FileDescriptorSet::decode(*encoded)?;
            all_fds.push(decoded);
        }

        let mut info = DescriptorInfo::default();

        for fds in all_fds {
            for fd in fds.file {
                let name = match fd.name.clone() {
                    None => {
                        return Err(Error::InvalidFileDescriptorSet("missing name".to_string()));
                    }
                    Some(n) => n,
                };

                if info.files.contains_key(&name) {
                    continue;
                }

                let fd = Arc::new(fd);
                info.files.insert(name, fd.clone());

                let result = DescriptorParser::process_file(fd)?;

                info.service_names.extend(result.service_names);
                info.symbols.extend(result.symbols.into_iter());
            }
        }

        Ok(info)
    }

    fn process_file(fd: Arc<FileDescriptorProto>) -> Result<File, Error> {
        let prefix = &fd.package.clone().unwrap_or_default();
        let mut service_names = vec![];
        let mut symbols = vec![];

        for msg in &fd.message_type {
            symbols.extend(DescriptorParser::process_message(fd.clone(), prefix, msg)?);
        }

        for en in &fd.enum_type {
            symbols.extend(DescriptorParser::process_enum(fd.clone(), prefix, en)?);
        }

        for service in &fd.service {
            let service_name =
                DescriptorParser::extract_name(prefix, "service", service.name.as_ref())?;
            service_names.push(service_name.clone());
            symbols.extend(vec![(service_name.clone(), fd.clone())]);

            for method in &service.method {
                let method_name =
                    DescriptorParser::extract_name(&service_name, "method", method.name.as_ref())?;
                symbols.extend(vec![(method_name, fd.clone())]);
            }
        }

        Ok(File {
            service_names,
            symbols,
        })
    }

    fn process_message(
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        msg: &DescriptorProto,
    ) -> Result<SymbolArray, Error> {
        let message_name = DescriptorParser::extract_name(prefix, "message", msg.name.as_ref())?;
        let mut symbols = vec![(message_name.clone(), fd.clone())];

        for nested in &msg.nested_type {
            symbols.extend(DescriptorParser::process_message(
                fd.clone(),
                &message_name,
                nested,
            )?);
        }

        for en in &msg.enum_type {
            symbols.extend(DescriptorParser::process_enum(
                fd.clone(),
                &message_name,
                en,
            )?);
        }

        for field in &msg.field {
            symbols.extend(DescriptorParser::process_field(
                fd.clone(),
                &message_name,
                field,
            )?);
        }

        for oneof in &msg.oneof_decl {
            let oneof_name =
                DescriptorParser::extract_name(&message_name, "oneof", oneof.name.as_ref())?;
            symbols.extend(vec![(oneof_name, fd.clone())]);
        }

        Ok(symbols)
    }

    fn process_enum(
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        en: &EnumDescriptorProto,
    ) -> Result<SymbolArray, Error> {
        let enum_name = DescriptorParser::extract_name(prefix, "enum", en.name.as_ref())?;

        let enums = (&en.value)
            .iter()
            .map(|value| {
                let value_name =
                    DescriptorParser::extract_name(&enum_name, "enum value", value.name.as_ref())?;
                Ok((value_name, fd.clone()))
            })
            .collect::<Result<Vec<(String, Arc<FileDescriptorProto>)>, Error>>()?;

        let symbols = vec![(enum_name.clone(), fd.clone())]
            .into_iter()
            .chain(enums.into_iter())
            .collect();

        Ok(symbols)
    }

    fn process_field(
        fd: Arc<FileDescriptorProto>,
        prefix: &str,
        field: &FieldDescriptorProto,
    ) -> Result<SymbolArray, Error> {
        let field_name = DescriptorParser::extract_name(prefix, "field", field.name.as_ref())?;
        Ok(vec![(field_name, fd)])
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
}

#[test]
fn test_parser() {
    let encoded_file_descriptor_sets: Vec<&[u8]> = Vec::new();
    let mut file_descriptor_sets: Vec<FileDescriptorSet> = Vec::new();

    file_descriptor_sets
        .push(FileDescriptorSet::decode(crate::pb::v1alpha::FILE_DESCRIPTOR_SET).unwrap());
    file_descriptor_sets
        .push(FileDescriptorSet::decode(crate::pb::v1::FILE_DESCRIPTOR_SET).unwrap());

    let info = DescriptorParser::process(encoded_file_descriptor_sets, file_descriptor_sets);

    assert!(info.is_ok());

    let info = info.unwrap();

    let mut service_names = info.service_names.clone();
    service_names.sort();

    let mut files = info.files.keys().collect::<Vec<_>>();
    files.sort();

    assert_eq!(
        service_names,
        [
            "grpc.reflection.v1.ServerReflection",
            "grpc.reflection.v1alpha.ServerReflection"
        ]
    );
    assert_eq!(files, ["reflection_v1.proto", "reflection_v1alpha.proto"]);

    assert!(info
        .symbols
        .contains_key("grpc.reflection.v1.ServerReflection"));
    assert!(info
        .symbols
        .contains_key("grpc.reflection.v1alpha.ServerReflection"));
}

#[test]
fn test_parser_encoded() {
    let mut encoded_file_descriptor_sets: Vec<&[u8]> = Vec::new();
    let file_descriptor_sets: Vec<FileDescriptorSet> = Vec::new();

    encoded_file_descriptor_sets.push(crate::pb::v1alpha::FILE_DESCRIPTOR_SET);
    encoded_file_descriptor_sets.push(crate::pb::v1::FILE_DESCRIPTOR_SET);

    let info = DescriptorParser::process(encoded_file_descriptor_sets, file_descriptor_sets);

    assert!(info.is_ok());

    let info = info.unwrap();

    let mut service_names = info.service_names.clone();
    service_names.sort();

    let mut files = info.files.keys().collect::<Vec<_>>();
    files.sort();

    assert_eq!(
        service_names,
        [
            "grpc.reflection.v1.ServerReflection",
            "grpc.reflection.v1alpha.ServerReflection"
        ]
    );
    assert_eq!(files, ["reflection_v1.proto", "reflection_v1alpha.proto"]);

    assert!(info
        .symbols
        .contains_key("grpc.reflection.v1.ServerReflection"));
    assert!(info
        .symbols
        .contains_key("grpc.reflection.v1alpha.ServerReflection"));
}
