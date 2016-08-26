use cpp_ffi_generator::{CppAndFfiData, CppFfiHeaderData};
use cpp_ffi_data::CppAndFfiMethod;
use cpp_type::{CppType, CppTypeBase, CppBuiltInNumericType, CppTypeIndirection,
               CppSpecificNumericTypeKind};
use cpp_ffi_data::{CppFfiType, IndirectionChange};
use rust_type::{RustName, RustType, CompleteType, RustTypeIndirection, RustFFIFunction,
                RustFFIArgument, RustToCTypeConversion};
use cpp_data::{CppTypeKind, EnumValue, CppTypeData};
use rust_info::{RustTypeDeclaration, RustTypeDeclarationKind, RustTypeWrapperKind, RustModule,
                RustMethod, RustMethodScope, RustMethodArgument, RustMethodArgumentsVariant,
                RustMethodArguments, TraitImpl, TraitName};
use cpp_method::ReturnValueAllocationPlace;
use cpp_ffi_data::CppFfiArgumentMeaning;
use utils::{CaseOperations, VecCaseOperations, WordIterator, add_to_multihash};
use log;

use std::collections::{HashMap, HashSet};

/// Mode of case conversion
enum Case {
  /// Class case: "OneTwo"
  Class,
  /// Snake case: "one_two"
  Snake,
}

/// If remove_qt_prefix is true, removes "Q" or "Qt"
/// if it is first word of the string and not the only one word.
/// Also converts case of the words.
fn remove_qt_prefix_and_convert_case(s: &String, case: Case, remove_qt_prefix: bool) -> String {
  let mut parts: Vec<_> = WordIterator::new(s).collect();
  if remove_qt_prefix && parts.len() > 1 {
    if parts[0] == "Q" || parts[0] == "q" || parts[0] == "Qt" {
      parts.remove(0);
    }
  }
  match case {
    Case::Snake => parts.to_snake_case(),
    Case::Class => parts.to_class_case(),
  }
}

/// Removes ".h" from include file name and performs the same
/// processing as remove_qt_prefix_and_convert_case() for snake case.
fn include_file_to_module_name(include_file: &String, remove_qt_prefix: bool) -> String {
  let mut r = include_file.clone();
  if r.ends_with(".h") {
    r = r[0..r.len() - 2].to_string();
  }
  remove_qt_prefix_and_convert_case(&r, Case::Snake, remove_qt_prefix)
}

/// Adds "_" to a string if it is a reserved word in Rust
#[cfg_attr(rustfmt, rustfmt_skip)]
fn sanitize_rust_identifier(name: &String) -> String {
  match name.as_ref() {
    "abstract" | "alignof" | "as" | "become" | "box" | "break" | "const" |
    "continue" | "crate" | "do" | "else" | "enum" | "extern" | "false" |
    "final" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" |
    "macro" | "match" | "mod" | "move" | "mut" | "offsetof" | "override" |
    "priv" | "proc" | "pub" | "pure" | "ref" | "return" | "Self" | "self" |
    "sizeof" | "static" | "struct" | "super" | "trait" | "true" | "type" |
    "typeof" | "unsafe" | "unsized" | "use" | "virtual" | "where" | "while" |
    "yield" => format!("{}_", name),
    _ => name.clone()
  }
}

pub struct RustGenerator {
  input_data: CppAndFfiData,
  config: RustGeneratorConfig,
  cpp_to_rust_type_map: HashMap<String, RustName>,
}

/// Results of adapting API for Rust wrapper.
/// This data is passed to Rust code generator.
pub struct RustGeneratorOutput {
  /// List of Rust modules to be generated.
  pub modules: Vec<RustModule>,
  /// List of FFI function imports to be generated.
  pub ffi_functions: HashMap<String, Vec<RustFFIFunction>>,
}

/// Config for rust_generator module.
pub struct RustGeneratorConfig {
  /// Name of generated crate
  pub crate_name: String,
  /// List of module names that should not be generated
  pub module_blacklist: Vec<String>,
  /// Flag instructing to remove leading "Q" and "Qt"
  /// from identifiers.
  pub remove_qt_prefix: bool,
}
// TODO: when supporting other libraries, implement removal of arbitrary prefixes

/// Execute processing
pub fn run(input_data: CppAndFfiData, config: RustGeneratorConfig) -> RustGeneratorOutput {
  let generator = RustGenerator {
    cpp_to_rust_type_map: generate_type_map(&input_data, &config),
    input_data: input_data,
    config: config,
  };
  let mut modules = Vec::new();
  for header in &generator.input_data.cpp_ffi_headers {
    if let Some(module) = generator.generate_modules_from_header(header) {
      modules.push(module);
    }
  }
  RustGeneratorOutput {
    ffi_functions: generator.ffi(),
    modules: modules,
  }
}

/// Generates RustName for specified function or type name,
/// including crate name and modules list.
fn calculate_rust_name(name: &String,
                       include_file: &String,
                       is_function: bool,
                       config: &RustGeneratorConfig)
                       -> RustName {
  let mut split_parts: Vec<_> = name.split("::").collect();
  let last_part = remove_qt_prefix_and_convert_case(&split_parts.pop().unwrap().to_string(),
                                                    if is_function {
                                                      Case::Snake
                                                    } else {
                                                      Case::Class
                                                    },
                                                    config.remove_qt_prefix);

  let mut parts = Vec::new();
  parts.push(config.crate_name.clone());
  parts.push(include_file_to_module_name(&include_file, config.remove_qt_prefix));
  for part in split_parts {
    parts.push(remove_qt_prefix_and_convert_case(&part.to_string(),
                                                 Case::Snake,
                                                 config.remove_qt_prefix));
  }

  if parts.len() > 2 && parts[1] == parts[2] {
    // special case
    parts.remove(2);
  }
  parts.push(last_part);
  RustName::new(parts)
}

fn generate_type_map(input_data: &CppAndFfiData,
                     config: &RustGeneratorConfig)
                     -> HashMap<String, RustName> {
  let mut map = HashMap::new();
  for type_info in &input_data.cpp_data.types {
    if let CppTypeKind::Class { size, .. } = type_info.kind {
      if size.is_none() {
        log::warning(format!("Rust type is not generated for a struct with unknown size: {}",
                             type_info.name));
        continue;
      }
    }
    if !map.contains_key(&type_info.name) {
      map.insert(type_info.name.clone(),
                 calculate_rust_name(&type_info.name, &type_info.include_file, false, config));
    }
  }
  for header in &input_data.cpp_ffi_headers {
    for method in &header.methods {
      if method.cpp_method.class_membership.is_none() {
        if !map.contains_key(&method.cpp_method.name) {
          map.insert(method.cpp_method.name.clone(),
                     calculate_rust_name(&method.cpp_method.name,
                                         &header.include_file,
                                         true,
                                         config));
        }
      }
    }
  }
  map
}

struct ProcessTypeResult {
  main_type: RustTypeDeclaration,
  overloading_types: Vec<RustTypeDeclaration>,
}
#[derive(Default)]
struct ProcessFunctionsResult {
  methods: Vec<RustMethod>,
  trait_impls: Vec<TraitImpl>,
  overloading_types: Vec<RustTypeDeclaration>,
}

impl RustGenerator {
  /// Generates CompleteType from CppFfiType, adding
  /// Rust API type, Rust FFI type and conversion between them.
  fn complete_type(&self,
                   cpp_ffi_type: &CppFfiType,
                   argument_meaning: &CppFfiArgumentMeaning)
                   -> Result<CompleteType, String> {
    let rust_ffi_type = try!(self.ffi_type(&cpp_ffi_type.ffi_type));
    let mut rust_api_type = rust_ffi_type.clone();
    let mut rust_api_to_c_conversion = RustToCTypeConversion::None;
    if let RustType::Common { ref mut indirection, .. } = rust_api_type {
      match cpp_ffi_type.conversion {
        IndirectionChange::NoChange => {
          if argument_meaning == &CppFfiArgumentMeaning::This {
            assert!(indirection == &RustTypeIndirection::Ptr);
            *indirection = RustTypeIndirection::Ref { lifetime: None };
            rust_api_to_c_conversion = RustToCTypeConversion::RefToPtr;
          }
        }
        IndirectionChange::ValueToPointer => {
          assert!(indirection == &RustTypeIndirection::Ptr);
          *indirection = RustTypeIndirection::None;
          rust_api_to_c_conversion = RustToCTypeConversion::ValueToPtr;
        }
        IndirectionChange::ReferenceToPointer => {
          assert!(indirection == &RustTypeIndirection::Ptr);
          *indirection = RustTypeIndirection::Ref { lifetime: None };
          rust_api_to_c_conversion = RustToCTypeConversion::RefToPtr;
        }
        IndirectionChange::QFlagsToUInt => {}
      }
    }
    if cpp_ffi_type.conversion == IndirectionChange::QFlagsToUInt {
      rust_api_to_c_conversion = RustToCTypeConversion::QFlagsToUInt;
      let enum_type = if let CppTypeBase::Class { ref template_arguments, .. } =
                             cpp_ffi_type.original_type.base {
        let args = template_arguments.as_ref().unwrap();
        assert!(args.len() == 1);
        if let CppTypeBase::Enum { ref name } = args[0].base {
          match self.cpp_to_rust_type_map.get(name) {
            None => return Err(format!("Type has no Rust equivalent: {}", name)),
            Some(rust_name) => rust_name.clone(),
          }
        } else {
          panic!("invalid original type for QFlags");
        }
      } else {
        panic!("invalid original type for QFlags");
      };
      rust_api_type = RustType::Common {
        base: RustName::new(vec!["qt_core".to_string(), "flags".to_string(), "QFlags".to_string()]),
        generic_arguments: Some(vec![RustType::Common {
                                       base: enum_type,
                                       generic_arguments: None,
                                       indirection: RustTypeIndirection::None,
                                       is_const: false,
                                     }]),
        indirection: RustTypeIndirection::None,
        is_const: false,
      }
    }

    Ok(CompleteType {
      cpp_ffi_type: cpp_ffi_type.ffi_type.clone(),
      cpp_type: cpp_ffi_type.original_type.clone(),
      cpp_to_ffi_conversion: cpp_ffi_type.conversion.clone(),
      rust_ffi_type: rust_ffi_type,
      rust_api_type: rust_api_type,
      rust_api_to_c_conversion: rust_api_to_c_conversion,
    })
  }

  /// Converts CppType to its exact Rust equivalent (FFI-compatible)
  fn ffi_type(&self, cpp_ffi_type: &CppType) -> Result<RustType, String> {
    let rust_name = match cpp_ffi_type.base {
      CppTypeBase::Void => {
        match cpp_ffi_type.indirection {
          CppTypeIndirection::None => return Ok(RustType::Void),
          _ => RustName::new(vec!["libc".to_string(), "c_void".to_string()]),
        }
      }
      CppTypeBase::BuiltInNumeric(ref numeric) => {
        if numeric == &CppBuiltInNumericType::Bool {
          RustName::new(vec!["bool".to_string()])
        } else {
          let own_name = match *numeric {
            CppBuiltInNumericType::Bool => unreachable!(),
            CppBuiltInNumericType::Char => "c_char",
            CppBuiltInNumericType::SChar => "c_schar",
            CppBuiltInNumericType::UChar => "c_uchar",
            CppBuiltInNumericType::WChar => "wchar_t",
            CppBuiltInNumericType::Short => "c_short",
            CppBuiltInNumericType::UShort => "c_ushort",
            CppBuiltInNumericType::Int => "c_int",
            CppBuiltInNumericType::UInt => "c_uint",
            CppBuiltInNumericType::Long => "c_long",
            CppBuiltInNumericType::ULong => "c_ulong",
            CppBuiltInNumericType::LongLong => "c_longlong",
            CppBuiltInNumericType::ULongLong => "c_ulonglong",
            CppBuiltInNumericType::Float => "c_float",
            CppBuiltInNumericType::Double => "c_double",
            _ => return Err(format!("unsupported numeric type: {:?}", numeric)),
          };
          RustName::new(vec!["libc".to_string(), own_name.to_string()])
        }
      }
      CppTypeBase::SpecificNumeric { ref bits, ref kind, .. } => {
        let letter = match *kind {
          CppSpecificNumericTypeKind::Integer { ref is_signed } => {
            if *is_signed { "i" } else { "u" }
          }
          CppSpecificNumericTypeKind::FloatingPoint => "f",
        };
        RustName::new(vec![format!("{}{}", letter, bits)])
      }
      CppTypeBase::PointerSizedInteger { ref is_signed, .. } => {
        RustName::new(vec![if *is_signed { "isize" } else { "usize" }.to_string()])
      }
      CppTypeBase::Enum { ref name } => {
        match self.cpp_to_rust_type_map.get(name) {
          None => return Err(format!("Type has no Rust equivalent: {}", name)),
          Some(rust_name) => rust_name.clone(),
        }
      }
      CppTypeBase::Class { ref name, ref template_arguments } => {
        if template_arguments.is_some() {
          return Err(format!("template types are not supported here yet"));
        }
        match self.cpp_to_rust_type_map.get(name) {
          None => return Err(format!("Type has no Rust equivalent: {}", name)),
          Some(rust_name) => rust_name.clone(),
        }
      }
      CppTypeBase::FunctionPointer { ref return_type,
                                     ref arguments,
                                     ref allows_variadic_arguments } => {
        if *allows_variadic_arguments {
          return Err(format!("Function pointers with variadic arguments are not supported"));
        }
        let mut rust_args = Vec::new();
        for arg in arguments {
          rust_args.push(try!(self.ffi_type(arg)));
        }
        let rust_return_type = try!(self.ffi_type(return_type));
        return Ok(RustType::FunctionPointer {
          arguments: rust_args,
          return_type: Box::new(rust_return_type),
        });
      }
      CppTypeBase::TemplateParameter { .. } => panic!("invalid cpp type"),
    };
    return Ok(RustType::Common {
      base: rust_name,
      is_const: cpp_ffi_type.is_const,
      indirection: match cpp_ffi_type.indirection {
        CppTypeIndirection::None => RustTypeIndirection::None,
        CppTypeIndirection::Ptr => RustTypeIndirection::Ptr,
        CppTypeIndirection::PtrPtr => RustTypeIndirection::PtrPtr,
        _ => return Err(format!("unsupported level of indirection: {:?}", cpp_ffi_type)),
      },
      generic_arguments: None,
    });
  }

  /// Generates exact Rust equivalent of CppAndFfiMethod object
  /// (FFI-compatible)
  fn ffi_function(&self, data: &CppAndFfiMethod) -> Result<RustFFIFunction, String> {
    let mut args = Vec::new();
    for arg in &data.c_signature.arguments {
      let rust_type = try!(self.ffi_type(&arg.argument_type.ffi_type));
      args.push(RustFFIArgument {
        name: sanitize_rust_identifier(&arg.name),
        argument_type: rust_type,
      });
    }
    Ok(RustFFIFunction {
      return_type: try!(self.ffi_type(&data.c_signature.return_type.ffi_type)),
      name: data.c_name.clone(),
      arguments: args,
    })
  }



  fn process_type(&self,
                  type_info: &CppTypeData,
                  c_header: &CppFfiHeaderData)
                  -> ProcessTypeResult {
    let rust_name = self.cpp_to_rust_type_map.get(&type_info.name).unwrap();
    match type_info.kind {
      CppTypeKind::Enum { ref values } => {
        let mut value_to_variant: HashMap<i64, EnumValue> = HashMap::new();
        for variant in values {
          let value = variant.value;
          if value_to_variant.contains_key(&value) {
            log::warning(format!("warning: {}: duplicated enum variant removed: {} \
                                  (previous variant: {})",
                                 type_info.name,
                                 variant.name,
                                 value_to_variant.get(&value).unwrap().name));
          } else {
            value_to_variant.insert(value,
                                    EnumValue {
                                      name: variant.name.to_class_case(),
                                      value: variant.value,
                                    });
          }
        }
        if value_to_variant.len() == 1 {
          let dummy_value = if value_to_variant.contains_key(&0) {
            1
          } else {
            0
          };
          value_to_variant.insert(dummy_value,
                                  EnumValue {
                                    name: "_Invalid".to_string(),
                                    value: dummy_value as i64,
                                  });
        }
        let mut values: Vec<_> = value_to_variant.into_iter()
          .map(|(_val, variant)| variant)
          .collect();
        values.sort_by(|a, b| a.value.cmp(&b.value));
        let mut is_flaggable = false;
        if let Some(instantiations) = self.input_data
          .cpp_data
          .template_instantiations
          .get(&"QFlags".to_string()) {
          let cpp_type_sample = CppType {
            is_const: false,
            indirection: CppTypeIndirection::None,
            base: CppTypeBase::Enum { name: type_info.name.clone() },
          };
          if instantiations.iter().find(|x| x.len() == 1 && &x[0] == &cpp_type_sample).is_some() {
            is_flaggable = true;
          }
        }
        ProcessTypeResult {
          main_type: RustTypeDeclaration {
            name: rust_name.last_name().clone(),
            kind: RustTypeDeclarationKind::CppTypeWrapper {
              kind: RustTypeWrapperKind::Enum {
                values: values,
                is_flaggable: is_flaggable,
              },
              cpp_type_name: type_info.name.clone(),
              cpp_template_arguments: None,
              methods: Vec::new(),
              traits: Vec::new(),
            },
          },
          overloading_types: Vec::new(),
        }
      }
      CppTypeKind::Class { ref size, .. } => {
        let methods_scope = RustMethodScope::Impl { type_name: rust_name.clone() };
        let functions_result = self.process_functions(c_header.methods
                                                        .iter()
                                                        .filter(|&x| {
                                                          x.cpp_method
                                                            .class_name() ==
                                                          Some(&type_info.name)
                                                        })
                                                        .collect(),
                                                      &methods_scope);

        ProcessTypeResult {
          main_type: RustTypeDeclaration {
            name: rust_name.last_name().clone(),
            kind: RustTypeDeclarationKind::CppTypeWrapper {
              kind: RustTypeWrapperKind::Struct { size: size.unwrap() },
              cpp_type_name: type_info.name.clone(),
              cpp_template_arguments: None,
              methods: functions_result.methods,
              traits: functions_result.trait_impls,
            },
          },
          overloading_types: functions_result.overloading_types,
        }
      }
    }
  }

  pub fn generate_modules_from_header(&self, c_header: &CppFfiHeaderData) -> Option<RustModule> {
    let module_name = include_file_to_module_name(&c_header.include_file,
                                                  self.config.remove_qt_prefix);
    if self.config.module_blacklist.iter().find(|&x| x == &module_name).is_some() {
      log::info(format!("Skipping module {}", module_name));
      return None;
    }
    let module_name1 = RustName::new(vec![self.config.crate_name.clone(), module_name]);
    return self.generate_module(c_header, &module_name1);
  }

  // TODO: check that all methods and types has been processed
  pub fn generate_module(&self,
                         c_header: &CppFfiHeaderData,
                         module_name: &RustName)
                         -> Option<RustModule> {
    log::info(format!("Generating Rust module {}", module_name.full_name(None)));

    let mut direct_submodules = HashSet::new();
    let mut rust_types = Vec::new();
    let mut rust_overloading_types = Vec::new();
    let mut good_methods = Vec::new();
    {
      let mut check_name = |name| {
        if let Some(rust_name) = self.cpp_to_rust_type_map.get(name) {
          let extra_modules_count = rust_name.parts.len() - module_name.parts.len();
          if extra_modules_count > 0 {
            if rust_name.parts[0..module_name.parts.len()] != module_name.parts[..] {
              return false; // not in this module
            }
          }
          if extra_modules_count == 2 {
            let direct_submodule = &rust_name.parts[module_name.parts.len()];
            if !direct_submodules.contains(direct_submodule) {
              direct_submodules.insert(direct_submodule.clone());
            }
          }
          if extra_modules_count == 1 {
            return true;
          }
          // this type is in nested submodule
        }
        false
      };
      for type_data in &self.input_data.cpp_data.types {
        if check_name(&type_data.name) {
          let mut result = self.process_type(type_data, c_header);
          rust_types.push(result.main_type);
          rust_overloading_types.append(&mut result.overloading_types);
        }
      }
      for method in &c_header.methods {
        if method.cpp_method.class_membership.is_none() {
          if check_name(&method.cpp_method.name) {
            good_methods.push(method);
          }
        }
      }
    }
    let mut submodules = Vec::new();
    for name in direct_submodules {
      let mut new_name = module_name.clone();
      new_name.parts.push(name);
      if let Some(m) = self.generate_module(c_header, &new_name) {
        submodules.push(m);
      }
    }
    let mut free_functions_result = self.process_functions(good_methods, &RustMethodScope::Free);
    assert!(free_functions_result.trait_impls.is_empty());
    rust_overloading_types.append(&mut free_functions_result.overloading_types);
    if rust_overloading_types.len() > 0 {
      submodules.push(RustModule {
        name: "overloading".to_string(),
        types: rust_overloading_types,
        functions: Vec::new(),
        submodules: Vec::new(),
      });
    }

    let module = RustModule {
      name: module_name.last_name().clone(),
      types: rust_types,
      functions: free_functions_result.methods,
      submodules: submodules,
    };
    return Some(module);
  }

  fn generate_function(&self,
                       method: &CppAndFfiMethod,
                       scope: &RustMethodScope)
                       -> Result<RustMethod, String> {
    if method.cpp_method.is_operator() {
      // TODO: implement operator traits
      return Err(format!("operators are not supported yet"));
    }
    let mut arguments = Vec::new();
    let mut return_type_info = None;
    for (arg_index, arg) in method.c_signature.arguments.iter().enumerate() {
      match self.complete_type(&arg.argument_type, &arg.meaning) {
        Ok(mut complete_type) => {
          if arg.meaning == CppFfiArgumentMeaning::ReturnValue {
            assert!(return_type_info.is_none());
            return_type_info = Some((complete_type, Some(arg_index as i32)));
          } else {
            if method.allocation_place == ReturnValueAllocationPlace::Heap &&
               method.cpp_method.is_destructor() {
              if let RustType::Common { ref mut indirection, .. } = complete_type.rust_api_type {
                assert!(*indirection == RustTypeIndirection::Ref { lifetime: None });
                *indirection = RustTypeIndirection::None;
              } else {
                panic!("unexpected void type");
              }
              assert!(complete_type.rust_api_to_c_conversion == RustToCTypeConversion::RefToPtr);
              complete_type.rust_api_to_c_conversion = RustToCTypeConversion::ValueToPtr;
            }

            arguments.push(RustMethodArgument {
              ffi_index: Some(arg_index as i32),
              argument_type: complete_type,
              name: if arg.meaning == CppFfiArgumentMeaning::This {
                "self".to_string()
              } else {
                sanitize_rust_identifier(&arg.name.to_snake_case())
              },
            });
          }
        }
        Err(msg) => {
          return Err(format!("Can't generate Rust method for method:\n{}\n{}\n",
                             method.short_text(),
                             msg));
        }
      }
    }
    if return_type_info.is_none() {
      match self.complete_type(&method.c_signature.return_type,
                               &CppFfiArgumentMeaning::ReturnValue) {
        Ok(mut r) => {
          if method.allocation_place == ReturnValueAllocationPlace::Heap &&
             !method.cpp_method.is_destructor() {
            if let RustType::Common { ref mut indirection, .. } = r.rust_api_type {
              assert!(*indirection == RustTypeIndirection::None);
              *indirection = RustTypeIndirection::Ptr;
            } else {
              panic!("unexpected void type");
            }
            assert!(r.cpp_type.indirection == CppTypeIndirection::None);
            assert!(r.cpp_to_ffi_conversion == IndirectionChange::ValueToPointer);
            assert!(r.rust_api_to_c_conversion == RustToCTypeConversion::ValueToPtr);
            r.rust_api_to_c_conversion = RustToCTypeConversion::None;

          }
          return_type_info = Some((r, None));
        }
        Err(msg) => {
          return Err(format!("Can't generate Rust method for method:\n{}\n{}\n",
                             method.short_text(),
                             msg));
        }
      }
    } else {
      assert!(method.c_signature.return_type == CppFfiType::void());
    }
    let return_type_info1 = return_type_info.unwrap();

    Ok(RustMethod {
      name: self.method_rust_name(method),
      scope: scope.clone(),
      arguments: RustMethodArguments::SingleVariant(RustMethodArgumentsVariant {
        arguments: arguments,
        cpp_method: method.clone(),
        return_type: return_type_info1.0,
        return_type_ffi_index: return_type_info1.1,
      }),
    })
  }

  fn method_rust_name(&self, method: &CppAndFfiMethod) -> RustName {
    let mut name = if method.cpp_method.class_membership.is_none() {
      self.cpp_to_rust_type_map.get(&method.cpp_method.name).unwrap().clone()
    } else {
      let x = if method.cpp_method.is_constructor() {
        "new".to_string()
      } else if method.cpp_method.is_destructor() {
        "delete".to_string()
      } else {
        method.cpp_method.name.to_snake_case()
      };
      RustName::new(vec![x])
    };
    let sanitized = sanitize_rust_identifier(name.last_name());
    if &sanitized != name.last_name() {
      name.parts.pop().unwrap();
      name.parts.push(sanitized);
    }
    name
  }

  fn process_functions(&self,
                       methods: Vec<&CppAndFfiMethod>,
                       scope: &RustMethodScope)
                       -> ProcessFunctionsResult {
    let mut single_rust_methods = Vec::new();
    let mut method_names = HashSet::new();
    let mut result = ProcessFunctionsResult::default();
    for method in &methods {
      if method.cpp_method.is_destructor() {
        if let &RustMethodScope::Impl { ref type_name } = scope {
          match method.allocation_place {
            ReturnValueAllocationPlace::Stack => {
              match self.generate_function(method, scope) {
                Ok(mut method) => {
                  method.name = RustName::new(vec!["drop".to_string()]);
                  method.scope = RustMethodScope::TraitImpl {
                    type_name: type_name.clone(),
                    trait_name: TraitName::Drop,
                  };
                  result.trait_impls.push(TraitImpl {
                    target_type: type_name.clone(),
                    trait_name: TraitName::Drop,
                    methods: vec![method],
                  });
                }
                Err(msg) => {
                  log::warning(format!("Failed to generate destructor: {}\n{:?}\n", msg, method))
                }
              }
              continue;
            }
            ReturnValueAllocationPlace::Heap => {
              result.trait_impls.push(TraitImpl {
                target_type: type_name.clone(),
                trait_name: TraitName::CppDeletable { deleter_name: method.c_name.clone() },
                methods: Vec::new(),
              });
              continue;
            }
            ReturnValueAllocationPlace::NotApplicable => {
              panic!("destructor must have allocation place")
            }
          }
        } else {
          panic!("destructor must be in class scope");
        }
      }

      match self.generate_function(method, scope) {
        Ok(rust_method) => {
          if !method_names.contains(rust_method.name.last_name()) {
            method_names.insert(rust_method.name.last_name().clone());
          }
          single_rust_methods.push(rust_method);
        }
        Err(msg) => log::warning(msg),
      }
    }
    // let mut name_counters = HashMap::new();
    for method_name in method_names {
      let current_methods: Vec<_> = single_rust_methods.clone()
        .into_iter()
        .filter(|m| m.name.last_name() == &method_name)
        .collect();
      let mut self_kind_to_methods: HashMap<_, Vec<_>> = HashMap::new();
      assert!(!current_methods.is_empty());
      for method in current_methods {
        add_to_multihash(&mut self_kind_to_methods, &method.self_arg_kind(), method);
      }
      let use_self_arg_caption = self_kind_to_methods.len() > 1;

      for (self_arg_kind, overloaded_methods) in self_kind_to_methods {

        let mut trait_name = method_name.clone();
        if use_self_arg_caption {
          trait_name = trait_name.clone() + &self_arg_kind.caption();
        }
        trait_name = trait_name.to_class_case() + "Params";
        if let &RustMethodScope::Impl { ref type_name } = scope {
          trait_name = format!("{}{}", type_name.last_name(), trait_name);
        }
        assert!(!overloaded_methods.is_empty());

        let mut all_real_args = HashMap::new();
        all_real_args.insert(ReturnValueAllocationPlace::Stack, HashSet::new());
        all_real_args.insert(ReturnValueAllocationPlace::Heap, HashSet::new());
        all_real_args.insert(ReturnValueAllocationPlace::NotApplicable, HashSet::new());
        let mut filtered_methods = Vec::new();
        for method in overloaded_methods {
          let ok = if let RustMethodArguments::SingleVariant(ref args) = method.arguments {
            let real_args: Vec<_> = args.arguments
              .iter()
              .map(|x| x.argument_type.rust_api_type.dealias_libc())
              .collect();
            if all_real_args.get_mut(&args.cpp_method.allocation_place)
              .unwrap()
              .contains(&real_args) {
              log::warning(format!("Removing method because another method with the same \
                                    argument types exists:\n{:?}",
                                   args.cpp_method.short_text()));
              false
            } else {
              all_real_args.get_mut(&args.cpp_method.allocation_place).unwrap().insert(real_args);
              true
            }
          } else {
            unreachable!()
          };
          if ok {
            filtered_methods.push(method);
          }
        }

        let methods_count = filtered_methods.len();
        let mut method = if methods_count > 1 {
          let first_method = filtered_methods[0].clone();
          let self_argument = if let RustMethodArguments::SingleVariant(ref args) =
                                     first_method.arguments {
            if args.arguments.len() > 0 && args.arguments[0].name == "self" {
              Some(args.arguments[0].clone())
            } else {
              None
            }
          } else {
            unreachable!()
          };
          let mut args_variants = Vec::new();
          for method in filtered_methods {
            assert!(method.name == first_method.name);
            assert!(method.scope == first_method.scope);
            if let RustMethodArguments::SingleVariant(mut args) = method.arguments {
              if let Some(ref self_argument) = self_argument {
                assert!(args.arguments.len() > 0 && &args.arguments[0] == self_argument);
                args.arguments.remove(0);
              }
              fn allocation_place_marker(marker_name: &'static str) -> RustMethodArgument {
                RustMethodArgument {
                  name: "allocation_place_marker".to_string(),
                  ffi_index: None,
                  argument_type: CompleteType {
                    cpp_type: CppType::void(),
                    cpp_ffi_type: CppType::void(),
                    cpp_to_ffi_conversion: IndirectionChange::NoChange,
                    rust_ffi_type: RustType::Void,
                    rust_api_type: RustType::Common {
                      base: RustName::new(vec!["cpp_box".to_string(), marker_name.to_string()]),
                      generic_arguments: None,
                      is_const: false,
                      indirection: RustTypeIndirection::None,
                    },
                    rust_api_to_c_conversion: RustToCTypeConversion::None,
                  },
                }
              }
              match args.cpp_method.allocation_place {
                ReturnValueAllocationPlace::Stack => {
                  args.arguments.push(allocation_place_marker("RustManaged"));
                }
                ReturnValueAllocationPlace::Heap => {
                  args.arguments.push(allocation_place_marker("CppPointer"));
                }
                ReturnValueAllocationPlace::NotApplicable => {}
              }
              args_variants.push(args);
            } else {
              unreachable!()
            }
          }

          // overloaded methods
          let shared_arguments = match self_argument {
            None => Vec::new(),
            Some(arg) => {
              let mut renamed_self = arg;
              renamed_self.name = "original_self".to_string();
              vec![renamed_self]
            }
          };
          let trait_lifetime = if shared_arguments.iter()
            .find(|x| x.argument_type.rust_api_type.is_ref())
            .is_some() {
            Some("a".to_string())
          } else {
            None
          };
          result.overloading_types.push(RustTypeDeclaration {
            name: trait_name.clone(),
            kind: RustTypeDeclarationKind::MethodParametersTrait {
              shared_arguments: shared_arguments.clone(),
              impls: args_variants,
              lifetime: trait_lifetime.clone(),
            },
          });
          RustMethod {
            name: first_method.name,
            scope: first_method.scope,
            arguments: RustMethodArguments::MultipleVariants {
              params_trait_name: trait_name.clone(),
              params_trait_lifetime: trait_lifetime,
              shared_arguments: shared_arguments,
              variant_argument_name: "params".to_string(),
            },
          }
        } else {
          filtered_methods.pop().unwrap()
        };
        if use_self_arg_caption {
          let name = method.name.parts.pop().unwrap();
          method.name.parts.push(format!("{}_{}", name, self_arg_kind.caption()));
        }
        result.methods.push(method);
      }
    }
    result
  }

  pub fn ffi(&self) -> HashMap<String, Vec<RustFFIFunction>> {
    log::info("Generating Rust FFI functions.");
    let mut ffi_functions = HashMap::new();

    for header in &self.input_data.cpp_ffi_headers {
      let mut functions = Vec::new();
      for method in &header.methods {
        match self.ffi_function(method) {
          Ok(function) => {
            functions.push(function);
          }
          Err(msg) => {
            log::warning(format!("Can't generate Rust FFI function for method:\n{}\n{}\n",
                                 method.short_text(),
                                 msg));
          }
        }
      }
      ffi_functions.insert(header.include_file.clone(), functions);
    }
    ffi_functions
  }
}

// TODO: sort types and methods before generating code

// ---------------------------------
#[test]
fn remove_qt_prefix_and_convert_case_test() {
  assert_eq!(remove_qt_prefix_and_convert_case(&"OneTwo".to_string(), Case::Class, false),
             "OneTwo");
  assert_eq!(remove_qt_prefix_and_convert_case(&"OneTwo".to_string(), Case::Snake, false),
             "one_two");
  assert_eq!(remove_qt_prefix_and_convert_case(&"OneTwo".to_string(), Case::Class, true),
             "OneTwo");
  assert_eq!(remove_qt_prefix_and_convert_case(&"OneTwo".to_string(), Case::Snake, true),
             "one_two");
  assert_eq!(remove_qt_prefix_and_convert_case(&"QDirIterator".to_string(), Case::Class, false),
             "QDirIterator");
  assert_eq!(remove_qt_prefix_and_convert_case(&"QDirIterator".to_string(), Case::Snake, false),
             "q_dir_iterator");
  assert_eq!(remove_qt_prefix_and_convert_case(&"QDirIterator".to_string(), Case::Class, true),
             "DirIterator");
  assert_eq!(remove_qt_prefix_and_convert_case(&"QDirIterator".to_string(), Case::Snake, true),
             "dir_iterator");
}

#[cfg(test)]
fn calculate_rust_name_test_part(name: &'static str,
                                 include_file: &'static str,
                                 is_function: bool,
                                 expected: &[&'static str]) {
  assert_eq!(calculate_rust_name(&name.to_string(),
                                 &include_file.to_string(),
                                 is_function,
                                 &RustGeneratorConfig {
                                   crate_name: "qt_core".to_string(),
                                   remove_qt_prefix: true,
                                   module_blacklist: Vec::new(),
                                 }),
             RustName::new(expected.into_iter().map(|x| x.to_string()).collect()));
}

#[test]
fn calculate_rust_name_test() {
  calculate_rust_name_test_part("myFunc1",
                                "QtGlobal",
                                true,
                                &["qt_core", "global", "my_func1"]);
  calculate_rust_name_test_part("QPointF",
                                "QPointF",
                                false,
                                &["qt_core", "point_f", "PointF"]);
  calculate_rust_name_test_part("QStringList::Iterator",
                                "QStringList",
                                false,
                                &["qt_core", "string_list", "Iterator"]);
  calculate_rust_name_test_part("QStringList::Iterator",
                                "QString",
                                false,
                                &["qt_core", "string", "string_list", "Iterator"]);
  calculate_rust_name_test_part("ns::func1",
                                "QRect",
                                true,
                                &["qt_core", "rect", "ns", "func1"]);
}
