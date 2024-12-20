use core::num;
use std::{
    borrow::{BorrowMut, Cow},
    collections::BTreeMap,
};

use figment::value::{Empty, Num, Tag};
use json::value::Index;
use schemars::schema::{
    InstanceType, RootSchema, Schema, SchemaObject, SingleOrVec, SubschemaValidation,
};

use super::*;

impl NodeContainer {
    pub fn from_json_schema(schema: &RootSchema) -> Self {
        schema_object_to_node("root", &schema.definitions, &schema.schema).unwrap()
    }
}

/// None means that the schema validate nothing
#[instrument(skip_all)]
pub(crate) fn schema_object_to_node(
    from: &str,
    def: &schemars::Map<String, Schema>,
    schema_object: &SchemaObject,
) -> Option<NodeContainer> {
    // debug!("enter function from {from}.\n{:#?}", schema_object);

    let mut res = NodeContainer::from_node(Node::Any);

    if let Some(single_or_vec) = &schema_object.instance_type {
        fn instance_type_to_node(instance_type: &InstanceType, format: Option<&String>) -> Node {
            match *instance_type {
                InstanceType::Null => Node::Null,
                InstanceType::Boolean => Node::Bool(NodeBool::new()),
                InstanceType::Object => Node::Object(NodeObject::new(IndexMap::new(), None)),
                InstanceType::Array => Node::Array(NodeArray::new_any()),
                InstanceType::Number => Node::Number(NodeNumber::new(
                    format
                        .and_then(|s| NumberValue::kind_from_str(s))
                        .unwrap_or(NumberValueLight::F64),
                )),
                InstanceType::String => Node::String(NodeString::new()),
                InstanceType::Integer => Node::Number(NodeNumber::new(
                    format
                        .and_then(|s| NumberValue::kind_from_str(s))
                        .unwrap_or(NumberValueLight::I128),
                )),
            }
        }

        let node = match single_or_vec {
            SingleOrVec::Single(instance_type) => NodeContainer::from_node(instance_type_to_node(
                instance_type,
                schema_object.format.as_ref(),
            )),
            SingleOrVec::Vec(vec) => {
                let nodes = vec
                    .iter()
                    .map(|instance_type| {
                        // xxx: why do we not pass metadata here ?

                        NodeContainer::from_node(instance_type_to_node(
                            instance_type,
                            schema_object.format.as_ref(),
                        ))
                    })
                    .collect();
                NodeContainer::from_node(Node::Enum(NodeEnum::new(nodes)))
            }
        };

        res = res.merge(&node)?;
    };

    if let Some(obj) = &schema_object.object {
        let mut nodes = IndexMap::new();

        for (name, type_definition) in &obj.properties {
            let node = schema_object_to_node("object", def, &type_definition.to_object())?;
            nodes.insert(name.clone(), node);
        }

        let additional_properties = if !obj.properties.is_empty() {
            None
        } else {
            obj.additional_properties
                .as_ref()
                .map(|additional_properties| {
                    schema_object_to_node("object", def, &additional_properties.to_object())
                })?
        };

        let node =
            NodeContainer::from_node(Node::Object(NodeObject::new(nodes, additional_properties)));

        res = res.merge(&node)?;
    }

    if let Some(enum_values) = &schema_object.enum_values {
        let node = if enum_values.len() == 1 {
            NodeContainer::from_node(Node::Value(NodeValue::new(enum_values[0].clone())))
        } else {
            let mut nodes = Vec::new();

            for value in enum_values {
                nodes.push(NodeContainer::from_node(Node::Value(NodeValue::new(
                    value.clone(),
                ))));
            }

            NodeContainer::from_node(Node::Enum(NodeEnum::new(nodes)))
        };

        res = res.merge(&node)?;
    }

    if let Some(array) = &schema_object.array {
        let template = match &array.items {
            Some(single_or_vec) => match single_or_vec {
                // this means items of the array all share the type described by this schema
                SingleOrVec::Single(schema) => {
                    let node = schema_object_to_node("array single", def, &schema.to_object())?;
                    NodeArrayTemplate::All(Box::new(node))
                }
                // items are of type array.
                SingleOrVec::Vec(vec) => {
                    let template: Option<Vec<_>> = vec
                        .iter()
                        .map(|schema| {
                            schema_object_to_node("array multiple", def, &schema.to_object())
                        })
                        .collect();

                    NodeArrayTemplate::FirstN(template?)
                }
            },
            None => NodeArrayTemplate::All(Box::new(NodeContainer::from_node(Node::Any))),
        };

        let node = NodeContainer::from_node(Node::Array(NodeArray {
            values: None,
            template,
            min: array.min_items,
            max: array.max_items,
        }));

        res = res.merge(&node)?;
    }

    if let Some(subschemas) = &schema_object.subschemas {
        if let Some(all_of) = &subschemas.all_of {
            let mut nodes = Vec::new();

            for schema in all_of {
                let node = schema_object_to_node("all_of", def, &schema.to_object())?;
                nodes.push(node);
            }

            let node = if nodes.len() > 1 {
                todo!()
            } else {
                nodes.remove(0)
            };
            res = res.merge(&node)?;
        }

        if let Some(one_of) = &subschemas.one_of {
            let mut nodes = Vec::new();
            for schema in one_of {
                let node = schema_object_to_node("one_of", def, &schema.to_object())?;

                nodes.push(node);
            }

            let node = NodeContainer::from_node(Node::Enum(NodeEnum::new(nodes)));
            res = res.merge(&node)?;
        }

        if let Some(any_of) = &subschemas.any_of {
            let mut nodes = Vec::new();
            for schema in any_of {
                let node = schema_object_to_node("one_of", def, &schema.to_object())?;

                nodes.push(node);
            }

            let node = NodeContainer::from_node(Node::Enum(NodeEnum::new(nodes)));
            res = res.merge(&node)?;
        }
    }

    if let Some(definition) = &schema_object.reference {
        if let Some(definition) = definition.strip_prefix("#/definitions/") {
            let schema = def.get(definition).unwrap();

            let node = schema_object_to_node("definition", def, &schema.to_object())?;
            res = res.merge(&node)?;
        }
    }

    let res = res.metadata(&schema_object.metadata);
    Some(res)
}

pub trait ToSchemaObject {
    fn to_object(&self) -> Cow<'_, SchemaObject>;
}

impl ToSchemaObject for Schema {
    fn to_object(&self) -> Cow<'_, SchemaObject> {
        match self {
            Schema::Object(o) => Cow::Borrowed(o),
            Schema::Bool(true) => Cow::Owned(SchemaObject::default()),
            Schema::Bool(false) => Cow::Owned(SchemaObject {
                subschemas: Some(Box::new(SubschemaValidation {
                    not: Some(Schema::Object(Default::default()).into()),
                    ..Default::default()
                })),
                ..Default::default()
            }),
        }
    }
}

pub(crate) fn json_value_to_figment_value(json_value: &json::Value) -> Value {
    match json_value {
        json::Value::Null => Value::Empty(Tag::Default, Empty::None),
        json::Value::Bool(value) => Value::Bool(Tag::Default, *value),
        json::Value::Number(number) => {
            let num = if let Some(n) = number.as_u64() {
                Num::U64(n)
            } else if let Some(n) = number.as_i64() {
                Num::I64(n)
            } else if let Some(n) = number.as_f64() {
                Num::F64(n)
            } else {
                panic!("not a valid number")
            };

            Value::Num(Tag::Default, num)
        }
        json::Value::String(str) => Value::String(Tag::Default, str.clone()),
        json::Value::Array(vec) => {
            let array = vec.iter().map(json_value_to_figment_value).collect();

            Value::Array(Tag::Default, array)
        }
        json::Value::Object(fields) => {
            let dict = fields
                .iter()
                .map(|(name, value)| (name.clone(), json_value_to_figment_value(value)))
                .collect();

            Value::Dict(Tag::Default, dict)
        }
    }
}

impl NodeContainer {
    fn merge(&self, other: &NodeContainer) -> Option<NodeContainer> {
        match (&self.node, &other.node) {
            (Node::Null, Node::Null) => Some(other.clone()),
            // (Node::Null, Node::Any) => Some(other.clone()),
            (Node::Bool(node_bool), Node::Null) => Some(other.clone()),
            (Node::Bool(node_bool), Node::Bool(node_bool2)) => Some(other.clone()),
            (Node::String(node_string), Node::String(node_string2)) => Some(other.clone()),
            (Node::Number(node_number), Node::Number(node_number2)) => Some(other.clone()),
            (Node::Object(node_object), Node::Object(node_object2)) => Some(other.clone()),
            (Node::Enum(node_enum1), Node::Enum(node_enum2)) => todo!("product?"),
            (Node::Enum(node_enum), node_other) => {
                match node_enum
                    .nodes
                    .iter()
                    .enumerate()
                    .find_map(|(pos, n)| n.merge(other).map(|n| (pos, n)))
                {
                    Some((pos, new)) => {
                        let mut node_self = self.clone();
                        let mut new_enum = node_enum.clone();
                        new_enum.nodes[pos] = new;
                        node_self.node = Node::Enum(new_enum);

                        Some(node_self)
                    }
                    None => None,
                }
            }
            // (node_self, Node::Enum(node_other)) => {
            //     todo!()
            // }
            (Node::Array(node_array), Node::Array(node_array2)) => Some(other.clone()),
            (_, Node::Value(node_value2)) => Some(other.clone()),
            (Node::Value(node_value1), _) => Some(self.clone()),
            (Node::Any, _) => Some(other.clone()),
            (_, Node::Any) => Some(self.clone()),
            (_, Node::Enum(node_enum)) => {
                match node_enum
                    .nodes
                    .iter()
                    .enumerate()
                    .find_map(|(pos, n)| n.merge(self).map(|n| (pos, n)))
                {
                    Some((pos, new)) => {
                        let mut node_other = other.clone();
                        let mut new_enum = node_enum.clone();
                        new_enum.nodes[pos] = new;
                        node_other.node = Node::Enum(new_enum);

                        Some(node_other)
                    }
                    None => None,
                }
            }
            _ => {
                warn!("none");
                dbg!(&self, &other);

                None
            }
        }
    }
}
