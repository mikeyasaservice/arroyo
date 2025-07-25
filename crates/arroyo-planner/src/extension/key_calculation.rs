use std::{fmt::Formatter, sync::Arc};

use arroyo_datastream::logical::{LogicalEdge, LogicalEdgeType, LogicalNode, OperatorName};
use arroyo_rpc::{
    df::{ArroyoSchema, ArroyoSchemaRef},
    grpc::api::KeyPlanOperator,
};
use datafusion::common::{internal_err, plan_err, DFSchemaRef, Result};

use datafusion::logical_expr::{Expr, LogicalPlan, UserDefinedLogicalNodeCore};
use datafusion_proto::{physical_plan::AsExecutionPlan, protobuf::PhysicalPlanNode};
use prost::Message;

use crate::{
    builder::{NamedNode, Planner},
    fields_with_qualifiers, multifield_partial_ord,
    physical::ArroyoPhysicalExtensionCodec,
    schema_from_df_fields_with_metadata,
};

use super::{ArroyoExtension, NodeWithIncomingEdges};

pub(crate) const KEY_CALCULATION_NAME: &str = "KeyCalculationExtension";

/* Calculation for computing keyed data, with a vec of keys
   that will be used for shuffling data to the correct nodes.

*/
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct KeyCalculationExtension {
    pub(crate) name: Option<String>,
    pub(crate) input: LogicalPlan,
    pub(crate) keys: Vec<usize>,
    pub(crate) schema: DFSchemaRef,
}

multifield_partial_ord!(KeyCalculationExtension, name, input, keys);

impl KeyCalculationExtension {
    pub fn new_named_and_trimmed(input: LogicalPlan, keys: Vec<usize>, name: String) -> Self {
        let output_fields: Vec<_> = fields_with_qualifiers(input.schema())
            .into_iter()
            .enumerate()
            .filter_map(|(index, field)| {
                if !keys.contains(&index) {
                    Some(field.clone())
                } else {
                    None
                }
            })
            .collect();
        let schema =
            schema_from_df_fields_with_metadata(&output_fields, input.schema().metadata().clone())
                .unwrap();
        Self {
            name: Some(name),
            input,
            keys,
            schema: Arc::new(schema),
        }
    }
    pub fn new(input: LogicalPlan, keys: Vec<usize>) -> Self {
        let schema = input.schema().clone();
        Self {
            name: None,
            input,
            keys,
            schema,
        }
    }
}

impl ArroyoExtension for KeyCalculationExtension {
    fn node_name(&self) -> Option<NamedNode> {
        None
    }

    fn plan_node(
        &self,
        planner: &Planner,
        index: usize,
        input_schemas: Vec<ArroyoSchemaRef>,
    ) -> Result<NodeWithIncomingEdges> {
        // check there's only one input
        if input_schemas.len() != 1 {
            return plan_err!("KeyCalculationExtension should have exactly one input");
        }
        let input_schema = input_schemas[0].clone();
        let physical_plan = planner.sync_plan(&self.input)?;

        let physical_plan_node: PhysicalPlanNode = PhysicalPlanNode::try_from_physical_plan(
            physical_plan,
            &ArroyoPhysicalExtensionCodec::default(),
        )?;
        let config = KeyPlanOperator {
            name: "key".into(),
            physical_plan: physical_plan_node.encode_to_vec(),
            key_fields: self.keys.iter().map(|k: &usize| *k as u64).collect(),
        };
        let node = LogicalNode::single(
            index as u32,
            format!("key_{index}"),
            OperatorName::ArrowKey,
            config.encode_to_vec(),
            format!("ArrowKey<{}>", config.name),
            1,
        );
        let edge = LogicalEdge::project_all(LogicalEdgeType::Forward, (*input_schema).clone());
        Ok(NodeWithIncomingEdges {
            node,
            edges: vec![edge],
        })
    }

    fn output_schema(&self) -> ArroyoSchema {
        let arrow_schema = Arc::new(self.input.schema().as_ref().into());
        ArroyoSchema::from_schema_keys(arrow_schema, self.keys.clone()).unwrap()
    }
}

impl UserDefinedLogicalNodeCore for KeyCalculationExtension {
    fn name(&self) -> &str {
        KEY_CALCULATION_NAME
    }

    fn inputs(&self) -> Vec<&LogicalPlan> {
        vec![&self.input]
    }

    fn schema(&self) -> &DFSchemaRef {
        &self.schema
    }

    fn expressions(&self) -> Vec<Expr> {
        vec![]
    }

    fn fmt_for_explain(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "KeyCalculationExtension: {}", self.schema())
    }

    fn with_exprs_and_inputs(&self, _exprs: Vec<Expr>, inputs: Vec<LogicalPlan>) -> Result<Self> {
        if inputs.len() != 1 {
            return internal_err!("input size inconsistent");
        }

        Ok(match self.name {
            Some(ref name) => {
                Self::new_named_and_trimmed(inputs[0].clone(), self.keys.clone(), name.clone())
            }
            None => Self::new(inputs[0].clone(), self.keys.clone()),
        })
    }
}
