use crate::extensions::BoxExtension;
use crate::registry::Registry;
use crate::{ErrorWithPosition, InputValueType, QueryError, Result, Type};
use bytes::Bytes;
use fnv::FnvHasher;
use graphql_parser::query::{
    Directive, Field, FragmentDefinition, SelectionSet, Value, VariableDefinition,
};
use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasherDefault;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::AtomicUsize;

/// Variables of query
#[derive(Debug, Clone)]
pub struct Variables(Value);

impl Default for Variables {
    fn default() -> Self {
        Self(Value::Object(Default::default()))
    }
}

impl Deref for Variables {
    type Target = BTreeMap<String, Value>;

    fn deref(&self) -> &Self::Target {
        if let Value::Object(obj) = &self.0 {
            obj
        } else {
            unreachable!()
        }
    }
}

impl DerefMut for Variables {
    fn deref_mut(&mut self) -> &mut Self::Target {
        if let Value::Object(obj) = &mut self.0 {
            obj
        } else {
            unreachable!()
        }
    }
}

impl Variables {
    /// Parse variables from JSON object.
    pub fn parse_from_json(value: serde_json::Value) -> Result<Self> {
        let gql_value = json_value_to_gql_value(value);
        if let Value::Object(_) = gql_value {
            Ok(Variables(gql_value))
        } else {
            Ok(Default::default())
        }
    }

    pub(crate) fn set_upload(
        &mut self,
        var_path: &str,
        filename: &str,
        content_type: Option<&str>,
        content: Bytes,
    ) {
        let mut it = var_path.split('.').peekable();

        if let Some(first) = it.next() {
            if first != "variables" {
                return;
            }
        }

        let mut current = &mut self.0;
        while let Some(s) = it.next() {
            let has_next = it.peek().is_some();

            if let Ok(idx) = s.parse::<i32>() {
                if let Value::List(ls) = current {
                    if let Some(value) = ls.get_mut(idx as usize) {
                        if !has_next {
                            *value = Value::String(file_string(filename, content_type, &content));
                            return;
                        } else {
                            current = value;
                        }
                    } else {
                        return;
                    }
                }
            } else if let Value::Object(obj) = current {
                if let Some(value) = obj.get_mut(s) {
                    if !has_next {
                        *value = Value::String(file_string(filename, content_type, &content));
                        return;
                    } else {
                        current = value;
                    }
                } else {
                    return;
                }
            }
        }
    }
}

fn file_string(filename: &str, content_type: Option<&str>, content: &[u8]) -> String {
    if let Some(content_type) = content_type {
        format!("file:{}:{}|", filename, content_type)
            + unsafe { std::str::from_utf8_unchecked(content) }
    } else {
        format!("file:{}|", filename) + unsafe { std::str::from_utf8_unchecked(content) }
    }
}

fn json_value_to_gql_value(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(n) => Value::Boolean(n),
        serde_json::Value::Number(n) if n.is_f64() => Value::Float(n.as_f64().unwrap()),
        serde_json::Value::Number(n) => Value::Int((n.as_i64().unwrap() as i32).into()),
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(ls) => {
            Value::List(ls.into_iter().map(json_value_to_gql_value).collect())
        }
        serde_json::Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(name, value)| (name, json_value_to_gql_value(value)))
                .collect(),
        ),
    }
}

#[derive(Default)]
pub struct Data(HashMap<TypeId, Box<dyn Any + Sync + Send>, BuildHasherDefault<FnvHasher>>);

impl Data {
    pub fn insert<D: Any + Send + Sync>(&mut self, data: D) {
        self.0.insert(TypeId::of::<D>(), Box::new(data));
    }
}

/// Context for `SelectionSet`
pub type ContextSelectionSet<'a> = ContextBase<'a, &'a SelectionSet>;

/// Context object for resolve field
pub type Context<'a> = ContextBase<'a, &'a Field>;

/// The query path segment
#[derive(Clone)]
pub enum QueryPathSegment<'a> {
    /// Index
    Index(usize),

    /// Field name
    Name(&'a str),
}

/// The query path node
#[derive(Clone)]
pub struct QueryPathNode<'a> {
    /// Parent node
    pub parent: Option<&'a QueryPathNode<'a>>,

    /// Current path segment
    pub segment: QueryPathSegment<'a>,
}

impl<'a> QueryPathNode<'a> {
    pub(crate) fn field_name(&self) -> &str {
        let mut p = self;
        loop {
            if let QueryPathSegment::Name(name) = &p.segment {
                return name;
            }
            p = p.parent.unwrap();
        }
    }

    pub(crate) fn for_each<F: FnMut(&QueryPathSegment<'a>)>(&self, mut f: F) {
        self.for_each_ref(&mut f);
    }

    fn for_each_ref<F: FnMut(&QueryPathSegment<'a>)>(&self, f: &mut F) {
        if let Some(parent) = &self.parent {
            parent.for_each_ref(f);
        }
        f(&self.segment);
    }
}

/// Query context
#[derive(Clone)]
pub struct ContextBase<'a, T> {
    pub(crate) path_node: Option<QueryPathNode<'a>>,
    pub(crate) resolve_id: &'a AtomicUsize,
    pub(crate) extensions: &'a [BoxExtension],
    pub(crate) item: T,
    pub(crate) variables: &'a Variables,
    pub(crate) variable_definitions: Option<&'a [VariableDefinition]>,
    pub(crate) registry: &'a Registry,
    pub(crate) data: &'a Data,
    pub(crate) fragments: &'a HashMap<String, FragmentDefinition>,
}

impl<'a, T> Deref for ContextBase<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

impl<'a, T> ContextBase<'a, T> {
    #[doc(hidden)]
    pub fn get_resolve_id(&self) -> usize {
        self.resolve_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[doc(hidden)]
    pub fn with_field(&'a self, field: &'a Field) -> ContextBase<'a, &'a Field> {
        ContextBase {
            path_node: Some(QueryPathNode {
                parent: self.path_node.as_ref(),
                segment: QueryPathSegment::Name(
                    field
                        .alias
                        .as_deref()
                        .unwrap_or_else(|| field.name.as_str()),
                ),
            }),
            extensions: self.extensions,
            item: field,
            resolve_id: self.resolve_id,
            variables: self.variables,
            variable_definitions: self.variable_definitions,
            registry: self.registry,
            data: self.data,
            fragments: self.fragments,
        }
    }

    #[doc(hidden)]
    pub fn with_selection_set(
        &self,
        selection_set: &'a SelectionSet,
    ) -> ContextBase<'a, &'a SelectionSet> {
        ContextBase {
            path_node: self.path_node.clone(),
            extensions: self.extensions,
            item: selection_set,
            resolve_id: self.resolve_id,
            variables: self.variables,
            variable_definitions: self.variable_definitions,
            registry: self.registry,
            data: self.data,
            fragments: self.fragments,
        }
    }

    /// Gets the global data defined in the `Schema`.
    pub fn data<D: Any + Send + Sync>(&self) -> &D {
        self.data
            .0
            .get(&TypeId::of::<D>())
            .and_then(|d| d.downcast_ref::<D>())
            .expect("The specified data type does not exist.")
    }

    fn var_value(&self, name: &str) -> Result<Value> {
        let def = self
            .variable_definitions
            .and_then(|defs| defs.iter().find(|def| def.name == name));
        if let Some(def) = def {
            if let Some(var_value) = self.variables.get(&def.name) {
                return Ok(var_value.clone());
            } else if let Some(default) = &def.default_value {
                return Ok(default.clone());
            }
        }
        Err(QueryError::VarNotDefined {
            var_name: name.to_string(),
        }
        .into())
    }

    fn resolve_input_value(&self, mut value: Value) -> Result<Value> {
        match value {
            Value::Variable(var_name) => self.var_value(&var_name),
            Value::List(ref mut ls) => {
                for value in ls {
                    if let Value::Variable(var_name) = value {
                        *value = self.var_value(&var_name)?;
                    }
                }
                Ok(value)
            }
            Value::Object(ref mut obj) => {
                for value in obj.values_mut() {
                    if let Value::Variable(var_name) = value {
                        *value = self.var_value(&var_name)?;
                    }
                }
                Ok(value)
            }
            _ => Ok(value),
        }
    }

    #[doc(hidden)]
    pub fn is_skip(&self, directives: &[Directive]) -> Result<bool> {
        for directive in directives {
            if directive.name == "skip" {
                if let Some(value) = directive
                    .arguments
                    .iter()
                    .find(|(name, _)| name == "if")
                    .map(|(_, value)| value)
                {
                    let value = self.resolve_input_value(value.clone())?;
                    let res: bool = InputValueType::parse(&value).ok_or_else(|| {
                        QueryError::ExpectedType {
                            expect: bool::qualified_type_name(),
                            actual: value,
                        }
                        .with_position(directive.position)
                    })?;
                    if res {
                        return Ok(true);
                    }
                } else {
                    return Err(QueryError::RequiredDirectiveArgs {
                        directive: "@skip",
                        arg_name: "if",
                        arg_type: "Boolean!",
                    }
                    .with_position(directive.position)
                    .into());
                }
            } else if directive.name == "include" {
                if let Some(value) = directive
                    .arguments
                    .iter()
                    .find(|(name, _)| name == "if")
                    .map(|(_, value)| value)
                {
                    let value = self.resolve_input_value(value.clone())?;
                    let res: bool = InputValueType::parse(&value).ok_or_else(|| {
                        QueryError::ExpectedType {
                            expect: bool::qualified_type_name(),
                            actual: value,
                        }
                        .with_position(directive.position)
                    })?;
                    if !res {
                        return Ok(true);
                    }
                } else {
                    return Err(QueryError::RequiredDirectiveArgs {
                        directive: "@include",
                        arg_name: "if",
                        arg_type: "Boolean!",
                    }
                    .with_position(directive.position)
                    .into());
                }
            } else {
                return Err(QueryError::UnknownDirective {
                    name: directive.name.clone(),
                }
                .with_position(directive.position)
                .into());
            }
        }

        Ok(false)
    }
}

impl<'a> ContextBase<'a, &'a SelectionSet> {
    #[doc(hidden)]
    pub fn with_index(&'a self, idx: usize) -> ContextBase<'a, &'a SelectionSet> {
        ContextBase {
            path_node: Some(QueryPathNode {
                parent: self.path_node.as_ref(),
                segment: QueryPathSegment::Index(idx),
            }),
            extensions: self.extensions,
            item: self.item,
            resolve_id: self.resolve_id,
            variables: self.variables,
            variable_definitions: self.variable_definitions,
            registry: self.registry,
            data: self.data,
            fragments: self.fragments,
        }
    }
}

impl<'a> ContextBase<'a, &'a Field> {
    #[doc(hidden)]
    pub fn param_value<T: InputValueType, F: FnOnce() -> Value>(
        &self,
        name: &str,
        default: F,
    ) -> Result<T> {
        match self
            .arguments
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .cloned()
        {
            Some(value) => {
                let value = self.resolve_input_value(value)?;
                let res = InputValueType::parse(&value).ok_or_else(|| {
                    QueryError::ExpectedType {
                        expect: T::qualified_type_name(),
                        actual: value,
                    }
                    .with_position(self.item.position)
                })?;
                Ok(res)
            }
            None => {
                let value = default();
                let res = InputValueType::parse(&value).ok_or_else(|| {
                    QueryError::ExpectedType {
                        expect: T::qualified_type_name(),
                        actual: value.clone(),
                    }
                    .with_position(self.item.position)
                })?;
                Ok(res)
            }
        }
    }

    #[doc(hidden)]
    pub fn result_name(&self) -> &str {
        self.item
            .alias
            .as_deref()
            .unwrap_or_else(|| self.item.name.as_str())
    }
}
