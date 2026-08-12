use crate::dsl::{
    Accessor, Array as ModelArray, ArrayElem as ModelArrayElem, BinOp, Binding as ModelBinding, Child as ModelChild,
    Choice as ModelChoice, CtxRef as ModelCtxRef, Field, Func as ModelFunc, Maybe as ModelMaybe, Model, NodeId,
    Number as ModelNumber, Object as ModelObject, ObjectField as ModelObjectField, Part as ModelPart, Range as ModelRange,
    RefId, Repeat as ModelRepeat, ResolvedRef, Selection, SpanFields as ModelSpanFields, SpanKind as ModelSpanKind, SrcRange,
    Step, Template as ModelTemplate, Trace as ModelTrace, UnaryOp, Value as ModelValue,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Beta, Exp, LogNormal, Normal, Pareto, Poisson};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;
use tiktoken_rs::CoreBPE;

// building the encoder parses the embedded vocab, do it once
static BPE: LazyLock<CoreBPE> = LazyLock::new(|| tiktoken_rs::o200k_base().expect("the embedded vocab parses"));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Plan {
    pub(super) events: Box<[EventPlan]>,
    pub(super) traces: Box<[Range<usize>]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct EventRef(pub(super) usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EventPlan {
    pub(super) root: EventRef,
    pub(super) parent: Option<EventRef>,
    pub(super) name: String,
    pub(super) kind: EventKind,
    pub(super) fields: EventFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EventKind {
    Task,
    Llm,
    Tool,
    Function,
}

impl EventKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            EventKind::Task => "task",
            EventKind::Llm => "llm",
            EventKind::Tool => "tool",
            EventKind::Function => "function",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EventFields {
    pub(super) input: Option<JsonValue>,
    pub(super) output: Option<JsonValue>,
    pub(super) expected: Option<JsonValue>,
    pub(super) error: Option<JsonValue>,
    pub(super) metadata: Option<JsonMap<String, JsonValue>>,
    pub(super) metrics: Option<JsonMap<String, JsonValue>>,
    pub(super) tags: Box<[String]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Planner {
    events: Vec<EventPlan>,
    traces: Vec<Range<usize>>,
}

// stable fnv-1a so seeds survive toolchain upgrades; std hashers make no
// cross-release guarantee
const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(hash: u64, value: u64) -> u64 {
    value
        .to_le_bytes()
        .iter()
        .fold(hash, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

fn mix_str(hash: u64, value: &str) -> u64 {
    value
        .bytes()
        .fold(hash, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

// slot discriminators: a draw is addressed by (instance path, slot), never by
// evaluation order, so edits to one expression leave every other draw alone
const COUNT_SALT: u64 = 0x01;
const CHANCE_SALT: u64 = 0x02;
const PICK_SALT: u64 = 0x03;
const FIELD_SALT: u64 = 0x10;
const KEY_SALT: u64 = 0x20;
const TAG_SALT: u64 = 0x30;
const BINDING_SALT: u64 = 0x40;

fn field_salt(field: Field) -> u64 {
    FIELD_SALT
        + match field {
            Field::Input => 0,
            Field::Output => 1,
            Field::Expected => 2,
            Field::Error => 3,
            Field::Metadata => 4,
            Field::Metrics => 5,
            Field::Tags => 6,
        }
}

// a lazily evaluated value; busy marks re-entry, which is a reference cycle
#[derive(Debug)]
enum BindSlot {
    Todo(ModelValue),
    Busy,
    Done(ModelValue),
}

#[derive(Debug)]
enum JsonSlot {
    Todo(ModelValue),
    Busy,
    Done(JsonValue),
}

#[derive(Debug)]
enum TagSlot {
    Todo(ModelTemplate),
    Busy,
    Done(String),
}

#[derive(Debug, Default)]
struct FieldSlots {
    input: Option<JsonSlot>,
    output: Option<JsonSlot>,
    expected: Option<JsonSlot>,
    error: Option<JsonSlot>,
    metadata: Option<Vec<(String, JsonSlot)>>,
    metrics: Option<Vec<(String, JsonSlot)>>,
    tags: Vec<TagSlot>,
}

fn field_slots(fields: &ModelSpanFields) -> FieldSlots {
    let slot = |value: &Option<ModelValue>| value.clone().map(JsonSlot::Todo);
    let keyed = |object: &Option<ModelObject>| {
        object.as_ref().map(|object| {
            object
                .elem
                .iter()
                .map(|field| (field.key.clone(), JsonSlot::Todo(field.value.clone())))
                .collect()
        })
    };
    FieldSlots {
        input: slot(&fields.input),
        output: slot(&fields.output),
        expected: slot(&fields.expected),
        error: slot(&fields.error),
        metadata: keyed(&fields.metadata),
        metrics: keyed(&fields.metrics),
        tags: fields.tags.iter().cloned().map(TagSlot::Todo).collect(),
    }
}

// one instantiation of a block for one generated trace; repeats instantiate a
// collection whose children are the iterations
#[derive(Debug)]
struct Instance {
    node: NodeId,
    parent: Option<usize>,
    children: Vec<usize>,
    shape: Shape,
    // the stable address of this instantiation, the basis for its slots' rngs
    path: u64,
    bindings: Vec<(String, BindSlot)>,
    fields: FieldSlots,
}

#[derive(Debug)]
enum Shape {
    Trace { name: String },
    Span { name: String, kind: EventKind },
    Collection,
    Iteration { index: usize, count: usize },
    // only the picked branch instantiates; a reference into another branch
    // finds no instance and reads as null
    Choice { pick: usize },
    Maybe { included: bool },
}

#[derive(Debug)]
struct Ctx<'m> {
    refs: &'m [ResolvedRef],
    trace_index: usize,
    // the address every instance path derives from: seed and trace index
    base: u64,
    // the rng of the slot evaluating right now, swapped as slots force
    rng: SmallRng,
    // the instance whose expression is evaluating right now
    site: usize,
    // the last reference crossed, for shape errors that surface downstream
    last_ref: Option<SrcRange>,
    instances: Vec<Instance>,
}

impl<'m> Ctx<'m> {
    fn new(refs: &'m [ResolvedRef], seed: u64, trace_index: usize) -> Self {
        let base = mix(mix(FNV_BASIS, seed), trace_index as u64);
        Self {
            refs,
            trace_index,
            base,
            rng: SmallRng::seed_from_u64(base),
            site: 0,
            last_ref: None,
            instances: Vec::new(),
        }
    }

    fn slot_rng(&self, path: u64, salt: u64) -> SmallRng {
        SmallRng::seed_from_u64(mix(path, salt))
    }

    // evaluates with the site and rng of a slot, restoring the previous ones
    fn scoped<T>(&mut self, site: usize, rng: SmallRng, eval: impl FnOnce(&mut Self) -> T) -> T {
        let outer_site = std::mem::replace(&mut self.site, site);
        let outer_rng = std::mem::replace(&mut self.rng, rng);
        let value = eval(self);
        self.site = outer_site;
        self.rng = outer_rng;
        value
    }

    fn add_instance(
        &mut self,
        node: NodeId,
        parent: Option<usize>,
        shape: Shape,
        salt: u64,
        bindings: &[ModelBinding],
        fields: FieldSlots,
    ) -> usize {
        let parent_path = parent.map_or(self.base, |parent| self.instances[parent].path);
        let path = mix(mix(parent_path, node.0 as u64), salt);
        let instance = Instance {
            node,
            parent,
            children: Vec::new(),
            shape,
            path,
            bindings: bindings
                .iter()
                .map(|binding| (binding.name.clone(), BindSlot::Todo(binding.value.clone())))
                .collect(),
            fields,
        };
        let index = self.instances.len();
        self.instances.push(instance);
        if let Some(parent) = parent {
            self.instances[parent].children.push(index);
        }
        index
    }

    // the root scope instantiates once per generated trace; its bindings ride
    // on the trace instance so references inside them resolve against it
    fn instantiate_trace(&mut self, trace: &ModelTrace, root: &[ModelBinding]) -> Result<(), Error> {
        let bindings: Vec<ModelBinding> = root.iter().chain(trace.bindings.iter()).cloned().collect();
        let instance = self.add_instance(
            trace.node,
            None,
            Shape::Trace {
                name: trace.name.clone(),
            },
            0,
            &bindings,
            field_slots(&trace.fields),
        );
        for child in &trace.children {
            self.instantiate_child(child, instance)?;
        }
        Ok(())
    }

    // structural decisions draw here, addressed by the block's own path so
    // they stay stable no matter what else changes
    fn instantiate_child(&mut self, child: &ModelChild, parent: usize) -> Result<(), Error> {
        match child {
            ModelChild::Span(span) => {
                let kind = match span.kind {
                    ModelSpanKind::Task => EventKind::Task,
                    ModelSpanKind::Llm => EventKind::Llm,
                    ModelSpanKind::Tool => EventKind::Tool,
                    ModelSpanKind::Function => EventKind::Function,
                };
                let instance = self.add_instance(
                    span.node,
                    Some(parent),
                    Shape::Span {
                        name: span.name.clone(),
                        kind,
                    },
                    0,
                    &span.bindings,
                    field_slots(&span.fields),
                );
                for child in &span.children {
                    self.instantiate_child(child, instance)?;
                }
                Ok(())
            }

            ModelChild::Repeat(ModelRepeat {
                node,
                count,
                count_range,
                bindings,
                children,
                ..
            }) => {
                let collection = self.add_instance(*node, Some(parent), Shape::Collection, 0, &[], FieldSlots::default());
                // count is drawn in the parent scope, bindings re-evaluate per iteration
                let rng = self.slot_rng(self.instances[collection].path, COUNT_SALT);
                let count = self.scoped(parent, rng, |ctx| eval_count(count.clone(), *count_range, ctx))?;
                for index in 0..count {
                    let iteration = self.add_instance(
                        *node,
                        Some(collection),
                        Shape::Iteration { index, count },
                        index as u64 + 1,
                        bindings,
                        FieldSlots::default(),
                    );
                    for child in children {
                        self.instantiate_child(child, iteration)?;
                    }
                }
                Ok(())
            }

            ModelChild::Choice(ModelChoice {
                node,
                bindings,
                children,
                ..
            }) => {
                let path = mix(mix(self.instances[parent].path, node.0 as u64), 0);
                let mut rng = self.slot_rng(path, PICK_SALT);
                let pick = rng.random_range(0..children.len());
                let instance = self.add_instance(
                    *node,
                    Some(parent),
                    Shape::Choice { pick },
                    0,
                    bindings,
                    FieldSlots::default(),
                );
                self.instantiate_child(&children[pick], instance)
            }

            ModelChild::Maybe(ModelMaybe {
                node,
                chance,
                chance_range,
                bindings,
                children,
                ..
            }) => {
                let path = mix(mix(self.instances[parent].path, node.0 as u64), 0);
                // chance is drawn in the parent scope, bindings evaluate only on inclusion
                let rng = self.slot_rng(path, CHANCE_SALT);
                let chance = self.scoped(parent, rng, |ctx| eval_chance(chance.clone(), *chance_range, ctx))?;
                let mut rng = self.slot_rng(path, PICK_SALT);
                let included = rng.random_bool(chance);
                let instance = self.add_instance(
                    *node,
                    Some(parent),
                    Shape::Maybe { included },
                    0,
                    bindings,
                    FieldSlots::default(),
                );
                if included {
                    for child in children {
                        self.instantiate_child(child, instance)?;
                    }
                }
                Ok(())
            }
        }
    }

    // one scope level up; collections are transparent, an iteration and its
    // repeat are the same scope
    fn scope_parent(&self, instance: usize) -> Option<usize> {
        let parent = self.instances[instance].parent?;
        match self.instances[parent].shape {
            Shape::Collection => self.instances[parent].parent,
            _ => Some(parent),
        }
    }

    fn ctx_value(&self, ctx_ref: ModelCtxRef) -> i64 {
        if ctx_ref == ModelCtxRef::TraceIndex {
            return self.trace_index as i64;
        }
        let mut at = Some(self.site);
        while let Some(instance) = at {
            if let Shape::Iteration { index, count } = self.instances[instance].shape {
                return match ctx_ref {
                    ModelCtxRef::RepeatIndex => index as i64,
                    ModelCtxRef::RepeatCount => count as i64,
                    ModelCtxRef::TraceIndex => unreachable!("trace index returned above"),
                };
            }
            at = self.instances[instance].parent;
        }
        unreachable!("modeler validated repeat refs are inside a repeat")
    }

    // a binding evaluates once, on first use, in its declaring scope
    fn force_binding(&mut self, name: &str) -> Result<ModelValue, Error> {
        let mut at = Some(self.site);
        while let Some(instance) = at {
            let found = self.instances[instance].bindings.iter().position(|(known, _)| known == name);
            if let Some(position) = found {
                return self.force_binding_slot(instance, position);
            }
            at = self.instances[instance].parent;
        }
        unreachable!("modeler guarantees references resolve to a binding in scope")
    }

    fn force_binding_slot(&mut self, instance: usize, position: usize) -> Result<ModelValue, Error> {
        match std::mem::replace(&mut self.instances[instance].bindings[position].1, BindSlot::Busy) {
            BindSlot::Done(value) => {
                self.instances[instance].bindings[position].1 = BindSlot::Done(value.clone());
                Ok(value)
            }
            BindSlot::Busy => Err(Error::new(
                ErrorKind::CircularReference,
                self.last_ref.unwrap_or(SrcRange::new(0, 0)),
            )),
            BindSlot::Todo(expr) => {
                let path = self.instances[instance].path;
                let name = &self.instances[instance].bindings[position].0;
                let rng = self.slot_rng(path, mix_str(BINDING_SALT, name));
                let value = self.scoped(instance, rng, |ctx| eval_binding(expr, ctx))?;
                self.instances[instance].bindings[position].1 = BindSlot::Done(value.clone());
                Ok(value)
            }
        }
    }

    // resolves a block reference to the referenced field's json value
    fn resolve_ref(&mut self, ref_id: RefId) -> Result<JsonValue, Error> {
        let reference = self.refs[ref_id.0 as usize].clone();
        self.last_ref = Some(reference.range);

        let mut at = self.site;
        for _ in 0..reference.up {
            at = self
                .scope_parent(at)
                .expect("modeler validated the up walk stays inside the trace");
        }

        self.apply_steps(at, &reference.steps, &reference.accessor, &reference.path, reference.range)
    }

    // walks the remaining steps from an instance; an iteration slice fans the
    // rest of the reference out over each iteration and collects an array
    fn apply_steps(
        &mut self,
        at: usize,
        steps: &[Step],
        accessor: &Accessor,
        path: &[Selection],
        range: SrcRange,
    ) -> Result<JsonValue, Error> {
        let Some((step, rest)) = steps.split_first() else {
            return self.read_accessor(at, accessor, path, range);
        };

        match step {
            Step::Child { candidates, position } => {
                let node = match position {
                    None => candidates[0],
                    Some(value) => {
                        // the position evaluates at the referencing site
                        let position = match eval_operand(value.clone(), self)? {
                            Scalar::Int(value) => value,
                            Scalar::Float(_) => return Err(Error::new(ErrorKind::NonIntegerIndex, range)),
                            _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
                        };
                        let position = usize::try_from(position)
                            .ok()
                            .filter(|&position| position < candidates.len())
                            .ok_or(Error::new(ErrorKind::IndexOutOfBounds, range))?;
                        candidates[position]
                    }
                };
                let child = self.instances[at]
                    .children
                    .iter()
                    .copied()
                    .find(|&child| self.instances[child].node == node);
                match child {
                    Some(child) => self.apply_steps(child, rest, accessor, path, range),
                    // an unpicked branch or a skipped maybe has no instance
                    None => match self.instances[at].shape {
                        Shape::Choice { .. } | Shape::Maybe { .. } => Ok(JsonValue::Null),
                        _ => unreachable!("spans of an instantiation always instantiate"),
                    },
                }
            }

            Step::Iteration(value) => {
                let iterations = self.instances[at].children.clone();
                let index = match eval_operand(value.clone(), self)? {
                    Scalar::Int(value) => value,
                    Scalar::Float(_) => return Err(Error::new(ErrorKind::NonIntegerIndex, range)),
                    _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
                };
                let index = usize::try_from(index)
                    .ok()
                    .filter(|&index| index < iterations.len())
                    .ok_or(Error::new(ErrorKind::IndexOutOfBounds, range))?;
                self.apply_steps(iterations[index], rest, accessor, path, range)
            }

            Step::Iterations { start, end } => {
                let iterations = self.instances[at].children.clone();
                let len = iterations.len();
                let start = match start {
                    Some(bound) => eval_slice_bound(bound.clone(), range, self)?.min(len),
                    None => 0,
                };
                let end = match end {
                    Some(bound) => eval_slice_bound(bound.clone(), range, self)?.min(len),
                    None => len,
                };
                let selected = if start >= end { &[][..] } else { &iterations[start..end] };
                let mut projected = Vec::with_capacity(selected.len());
                for &iteration in selected {
                    projected.push(self.apply_steps(iteration, rest, accessor, path, range)?);
                }
                Ok(JsonValue::Array(projected))
            }
        }
    }

    fn read_accessor(
        &mut self,
        at: usize,
        accessor: &Accessor,
        path: &[Selection],
        range: SrcRange,
    ) -> Result<JsonValue, Error> {
        match accessor {
            Accessor::Field(field) => self.read_field(at, *field, path.to_vec(), range),
            // the current iteration of the repeat collection `at`, found on
            // the referencing site's own chain
            Accessor::Index | Accessor::Count => {
                let mut cursor = Some(self.site);
                while let Some(instance) = cursor {
                    if let Shape::Iteration { index, count } = self.instances[instance].shape
                        && self.instances[instance].parent == Some(at)
                    {
                        let value = match accessor {
                            Accessor::Index => index,
                            _ => count,
                        };
                        return Ok(JsonValue::Number((value as i64).into()));
                    }
                    cursor = self.instances[instance].parent;
                }
                unreachable!("modeler validated the named repeat encloses the reference")
            }
            Accessor::Chosen => match self.instances[at].shape {
                Shape::Choice { pick } => Ok(JsonValue::Number((pick as i64).into())),
                _ => unreachable!("modeler validated chosen against a choice"),
            },
            Accessor::Included => match self.instances[at].shape {
                Shape::Maybe { included } => Ok(JsonValue::Bool(included)),
                _ => unreachable!("modeler validated included against a maybe"),
            },
        }
    }

    fn read_field(&mut self, instance: usize, field: Field, path: Vec<Selection>, at: SrcRange) -> Result<JsonValue, Error> {
        let mut selections = path.into_iter().peekable();

        let json = match field {
            Field::Input | Field::Output | Field::Expected | Field::Error => self.force_field(instance, field, at)?,
            Field::Metadata | Field::Metrics => {
                // a leading string selection reads just its key, so sibling
                // keys of the same object stay independent slots
                match selections.peek() {
                    Some(Selection::Index(_)) => {
                        let Some(Selection::Index(value)) = selections.next() else {
                            unreachable!("the peeked selection is an index");
                        };
                        let Scalar::Str(key) = eval_operand(value, self)? else {
                            return Err(Error::new(ErrorKind::RefShapeMismatch, at));
                        };
                        self.force_key(instance, field, &key, at)?
                    }
                    _ => self.force_object(instance, field, at)?,
                }
            }
            Field::Tags => {
                let count = self.instances[instance].fields.tags.len();
                let mut tags = Vec::with_capacity(count);
                for index in 0..count {
                    tags.push(JsonValue::String(self.force_tag(instance, index, at)?));
                }
                JsonValue::Array(tags)
            }
        };

        let mut json = json;
        for selection in selections {
            json = json_select(json, selection, at, self)?;
        }
        Ok(json)
    }

    fn force_field(&mut self, instance: usize, field: Field, at: SrcRange) -> Result<JsonValue, Error> {
        let slot = {
            let fields = &mut self.instances[instance].fields;
            let slot = match field {
                Field::Input => &mut fields.input,
                Field::Output => &mut fields.output,
                Field::Expected => &mut fields.expected,
                Field::Error => &mut fields.error,
                _ => unreachable!("keyed and tag fields force through their own paths"),
            };
            // only a dynamic position can reach a block without the field
            let Some(slot) = slot.as_mut() else {
                return Err(Error::new(ErrorKind::AbsentRefField, at));
            };
            std::mem::replace(slot, JsonSlot::Busy)
        };

        let done = match slot {
            JsonSlot::Done(json) => json,
            JsonSlot::Busy => return Err(Error::new(ErrorKind::CircularReference, at)),
            JsonSlot::Todo(expr) => {
                let rng = self.slot_rng(self.instances[instance].path, field_salt(field));
                self.scoped(instance, rng, |ctx| lower_value(expr, ctx))?
            }
        };

        let fields = &mut self.instances[instance].fields;
        let slot = match field {
            Field::Input => &mut fields.input,
            Field::Output => &mut fields.output,
            Field::Expected => &mut fields.expected,
            Field::Error => &mut fields.error,
            _ => unreachable!("keyed and tag fields force through their own paths"),
        };
        *slot.as_mut().expect("the slot was just taken") = JsonSlot::Done(done.clone());
        Ok(done)
    }

    fn force_key(&mut self, instance: usize, field: Field, key: &str, at: SrcRange) -> Result<JsonValue, Error> {
        fn keyed(fields: &mut FieldSlots, field: Field) -> Option<&mut Vec<(String, JsonSlot)>> {
            match field {
                Field::Metadata => fields.metadata.as_mut(),
                Field::Metrics => fields.metrics.as_mut(),
                _ => unreachable!("only keyed fields force by key"),
            }
        }

        let (position, slot) = {
            let Some(entries) = keyed(&mut self.instances[instance].fields, field) else {
                return Err(Error::new(ErrorKind::AbsentRefField, at));
            };
            let Some(position) = entries.iter().position(|(known, _)| known == key) else {
                return Err(Error::new(ErrorKind::MissingObjectKey, at));
            };
            (position, std::mem::replace(&mut entries[position].1, JsonSlot::Busy))
        };

        let done = match slot {
            JsonSlot::Done(json) => json,
            JsonSlot::Busy => return Err(Error::new(ErrorKind::CircularReference, at)),
            JsonSlot::Todo(expr) => {
                let salt = mix_str(mix(field_salt(field), KEY_SALT), key);
                let rng = self.slot_rng(self.instances[instance].path, salt);
                self.scoped(instance, rng, |ctx| lower_value(expr, ctx))?
            }
        };

        let entries = keyed(&mut self.instances[instance].fields, field).expect("the entries were just taken");
        entries[position].1 = JsonSlot::Done(done.clone());
        Ok(done)
    }

    fn force_object(&mut self, instance: usize, field: Field, at: SrcRange) -> Result<JsonValue, Error> {
        let keys: Vec<String> = {
            let fields = &self.instances[instance].fields;
            let entries = match field {
                Field::Metadata => fields.metadata.as_ref(),
                Field::Metrics => fields.metrics.as_ref(),
                _ => unreachable!("only keyed fields force as objects"),
            };
            let Some(entries) = entries else {
                return Err(Error::new(ErrorKind::AbsentRefField, at));
            };
            entries.iter().map(|(key, _)| key.clone()).collect()
        };

        let mut object = JsonMap::with_capacity(keys.len());
        for key in keys {
            let value = self.force_key(instance, field, &key, at)?;
            object.insert(key, value);
        }
        Ok(JsonValue::Object(object))
    }

    fn force_tag(&mut self, instance: usize, index: usize, at: SrcRange) -> Result<String, Error> {
        let slot = std::mem::replace(&mut self.instances[instance].fields.tags[index], TagSlot::Busy);
        let done = match slot {
            TagSlot::Done(text) => text,
            TagSlot::Busy => return Err(Error::new(ErrorKind::CircularReference, at)),
            TagSlot::Todo(template) => {
                let rng = self.slot_rng(self.instances[instance].path, mix(TAG_SALT, index as u64));
                self.scoped(instance, rng, |ctx| resolve_template(template, ctx))?
            }
        };
        self.instances[instance].fields.tags[index] = TagSlot::Done(done.clone());
        Ok(done)
    }

    // forces every field of an event instance, in declaration order
    fn event_fields(&mut self, instance: usize) -> Result<EventFields, Error> {
        let at = SrcRange::new(0, 0);
        let scalar = |ctx: &mut Self, field: Field| -> Result<Option<JsonValue>, Error> {
            match ctx.force_field(instance, field, at) {
                Ok(json) => Ok(Some(json)),
                Err(error) if error.kind == ErrorKind::AbsentRefField => Ok(None),
                Err(error) => Err(error),
            }
        };
        let keyed = |ctx: &mut Self, field: Field| -> Result<Option<JsonMap<String, JsonValue>>, Error> {
            match ctx.force_object(instance, field, at) {
                Ok(JsonValue::Object(object)) => Ok(Some(object)),
                Ok(_) => unreachable!("forcing an object field yields an object"),
                Err(error) if error.kind == ErrorKind::AbsentRefField => Ok(None),
                Err(error) => Err(error),
            }
        };

        let input = scalar(self, Field::Input)?;
        let output = scalar(self, Field::Output)?;
        let expected = scalar(self, Field::Expected)?;
        let error = scalar(self, Field::Error)?;
        let metadata = keyed(self, Field::Metadata)?;
        let metrics = keyed(self, Field::Metrics)?;
        let count = self.instances[instance].fields.tags.len();
        let mut tags = Vec::with_capacity(count);
        for index in 0..count {
            tags.push(self.force_tag(instance, index, at)?);
        }

        Ok(EventFields {
            input,
            output,
            expected,
            error,
            metadata,
            metrics,
            tags: tags.into_boxed_slice(),
        })
    }
}

// selections drill into a referenced field's json value
fn json_select(json: JsonValue, selection: Selection, at: SrcRange, ctx: &mut Ctx) -> Result<JsonValue, Error> {
    match selection {
        Selection::Index(value) => match (json, eval_operand(value, ctx)?) {
            (JsonValue::Array(mut elems), Scalar::Int(position)) => usize::try_from(position)
                .ok()
                .filter(|&position| position < elems.len())
                .map(|position| elems.swap_remove(position))
                .ok_or(Error::new(ErrorKind::IndexOutOfBounds, at)),
            (JsonValue::Array(_), Scalar::Float(_)) => Err(Error::new(ErrorKind::NonIntegerIndex, at)),
            (JsonValue::Object(mut map), Scalar::Str(key)) => {
                map.remove(&key).ok_or(Error::new(ErrorKind::MissingObjectKey, at))
            }
            _ => Err(Error::new(ErrorKind::RefShapeMismatch, at)),
        },
        Selection::Slice { start, end } => {
            let JsonValue::Array(elems) = json else {
                return Err(Error::new(ErrorKind::RefShapeMismatch, at));
            };
            let len = elems.len();
            let start = match start {
                Some(bound) => eval_slice_bound(bound, at, ctx)?.min(len),
                None => 0,
            };
            let end = match end {
                Some(bound) => eval_slice_bound(bound, at, ctx)?.min(len),
                None => len,
            };
            let elems = if start >= end {
                Vec::new()
            } else {
                elems.into_iter().skip(start).take(end - start).collect()
            };
            Ok(JsonValue::Array(elems))
        }
    }
}

// splices dynamic spreads in place; items stay lazy like untaken branches
fn splice_array(elem: Vec<ModelArrayElem>, ctx: &mut Ctx) -> Result<Vec<ModelValue>, Error> {
    let mut items = Vec::with_capacity(elem.len());
    for entry in elem {
        match entry {
            ModelArrayElem::Item(value) => items.push(value),
            ModelArrayElem::Spread(value) => match eval_container(value, ctx)? {
                ModelValue::Array(ModelArray { elem }) => items.extend(splice_array(elem, ctx)?),
                _ => return Err(shape_error(ctx)),
            },
        }
    }
    Ok(items)
}

// a referenced value re-enters evaluation as a constant model value
fn json_to_value(json: JsonValue) -> ModelValue {
    match json {
        JsonValue::Null => ModelValue::Null,
        JsonValue::Bool(value) => ModelValue::Bool(value),
        JsonValue::Number(number) => match number.as_i64() {
            Some(value) => ModelValue::Num(ModelNumber::Int(value)),
            None => ModelValue::Num(ModelNumber::Float(number.as_f64().expect("json numbers convert to floats"))),
        },
        JsonValue::String(value) => ModelValue::Str(value),
        JsonValue::Array(elems) => ModelValue::Array(ModelArray {
            elem: elems
                .into_iter()
                .map(|value| ModelArrayElem::Item(json_to_value(value)))
                .collect(),
        }),
        JsonValue::Object(map) => ModelValue::Object(ModelObject {
            elem: map
                .into_iter()
                .map(|(key, value)| ModelObjectField {
                    key,
                    value: json_to_value(value),
                })
                .collect(),
        }),
    }
}

impl Planner {
    // emission stays strictly pre-order: the materializer's timing and
    // last-descendant scans depend on parents preceding children
    fn emit_trace(&mut self, ctx: &mut Ctx) -> Result<(), Error> {
        let start = self.events.len();
        let root = EventRef(start);
        self.emit_instance(0, root, None, ctx)?;
        self.traces.push(start..self.events.len());
        Ok(())
    }

    fn emit_instance(&mut self, instance: usize, root: EventRef, parent: Option<EventRef>, ctx: &mut Ctx) -> Result<(), Error> {
        let event = match &ctx.instances[instance].shape {
            Shape::Trace { name } => Some((name.clone(), EventKind::Task)),
            Shape::Span { name, kind } => Some((name.clone(), *kind)),
            // dynamic blocks are structurally transparent
            Shape::Collection | Shape::Iteration { .. } | Shape::Choice { .. } | Shape::Maybe { .. } => None,
        };

        let parent = match event {
            Some((name, kind)) => {
                let event_ref = EventRef(self.events.len());
                let fields = ctx.event_fields(instance)?;
                self.events.push(EventPlan {
                    root,
                    parent,
                    name,
                    kind,
                    fields,
                });
                Some(event_ref)
            }
            None => parent,
        };

        let children = ctx.instances[instance].children.clone();
        for child in children {
            self.emit_instance(child, root, parent, ctx)?;
        }
        Ok(())
    }
}

fn eval_count(count: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<usize, Error> {
    match eval_operand(count, ctx)? {
        Scalar::Int(value) => usize::try_from(value).map_err(|_| Error::new(ErrorKind::NegativeRepeatCount, range)),
        Scalar::Float(_) => Err(Error::new(ErrorKind::NonIntegerRepeatCount, range)),
        _ => unreachable!("modeler validated the count as a number"),
    }
}

fn eval_chance(chance: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<f64, Error> {
    let value = match eval_operand(chance, ctx)? {
        Scalar::Int(value) => value as f64,
        Scalar::Float(value) => value,
        _ => unreachable!("modeler validated the chance as a number"),
    };

    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(ErrorKind::ChanceOutOfRange, range))
    }
}

// dynamic blocks make these estimates, only used as allocation hints
fn trace_len(trace: &ModelTrace) -> usize {
    1 + trace.children.iter().map(child_len).sum::<usize>()
}
fn child_len(child: &ModelChild) -> usize {
    match child {
        ModelChild::Span(span) => 1 + span.children.iter().map(child_len).sum::<usize>(),
        // one iteration or inclusion
        ModelChild::Repeat(repeat) => repeat.children.iter().map(child_len).sum(),
        ModelChild::Maybe(maybe) => maybe.children.iter().map(child_len).sum(),
        ModelChild::Choice(choice) => choice.children.iter().map(child_len).max().unwrap_or(0),
    }
}

// fully evaluates a scope binding to a constant value the environment can hold
fn eval_binding(value: ModelValue, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let value = match value {
        ModelValue::Str(_) | ModelValue::Num(_) | ModelValue::Bool(_) | ModelValue::Null => value,
        ModelValue::Template(template) => ModelValue::Str(resolve_template(template, ctx)?),
        ModelValue::VarRef(name) => ctx.force_binding(&name)?,
        ModelValue::CtxRef(ctx_ref) => ModelValue::Num(ModelNumber::Int(ctx.ctx_value(ctx_ref))),
        ModelValue::BlockRef { ref_id, .. } => json_to_value(ctx.resolve_ref(ref_id)?),

        ModelValue::Array(ModelArray { elem }) => ModelValue::Array(ModelArray {
            elem: splice_array(elem, ctx)?
                .into_iter()
                .map(|value| Ok(ModelArrayElem::Item(eval_binding(value, ctx)?)))
                .collect::<Result<_, _>>()?,
        }),

        ModelValue::Object(ModelObject { elem }) => ModelValue::Object(ModelObject {
            elem: elem
                .into_iter()
                .map(|ModelObjectField { key, value }| {
                    Ok(ModelObjectField {
                        key,
                        value: eval_binding(value, ctx)?,
                    })
                })
                .collect::<Result<_, _>>()?,
        }),

        // a pick may itself still be dynamic, so recurse on the result
        ModelValue::Func { func, range } => eval_binding(eval_func(func, range, ctx)?, ctx)?,

        ModelValue::Unary { op, operand, range } => {
            let operand = eval_operand(*operand, ctx)?;
            scalar_to_value(eval_unary(op, operand, range)?)
        }
        ModelValue::Binary { op, lhs, rhs, range } => scalar_to_value(eval_binary(op, *lhs, *rhs, range, ctx)?),

        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool(ctx)? {
                then
            } else {
                otherwise
            };
            eval_binding(*taken, ctx)?
        }

        ModelValue::Index { target, index, range } => eval_binding(eval_index(*target, *index, range, ctx)?, ctx)?,

        ModelValue::Slice {
            target,
            start,
            end,
            range,
        } => eval_binding(eval_slice(*target, start, end, range, ctx)?, ctx)?,
    };

    Ok(value)
}

fn scalar_to_value(scalar: Scalar) -> ModelValue {
    match scalar {
        Scalar::Int(value) => ModelValue::Num(ModelNumber::Int(value)),
        Scalar::Float(value) => ModelValue::Num(ModelNumber::Float(value)),
        Scalar::Bool(value) => ModelValue::Bool(value),
        Scalar::Str(value) => ModelValue::Str(value),
    }
}

fn lower_value(value: ModelValue, ctx: &mut Ctx) -> Result<JsonValue, Error> {
    let value = match value {
        ModelValue::Str(value) => JsonValue::String(value),
        ModelValue::Template(template) => JsonValue::String(resolve_template(template, ctx)?),
        ModelValue::Bool(value) => JsonValue::Bool(value),
        ModelValue::Null => JsonValue::Null,

        // the stored value is constant, so lowering it is pure conversion
        ModelValue::VarRef(name) => lower_value(ctx.force_binding(&name)?, ctx)?,

        ModelValue::CtxRef(ctx_ref) => JsonValue::Number(ctx.ctx_value(ctx_ref).into()),

        // a referenced field is already json
        ModelValue::BlockRef { ref_id, .. } => ctx.resolve_ref(ref_id)?,

        ModelValue::Num(ModelNumber::Int(value)) => JsonValue::Number(value.into()),

        ModelValue::Num(ModelNumber::Float(value)) => {
            let number = JsonNumber::from_f64(value).expect("modeler guarantees finite floating-point numbers");
            JsonValue::Number(number)
        }

        ModelValue::Array(ModelArray { elem }) => JsonValue::Array(
            splice_array(elem, ctx)?
                .into_iter()
                .map(|value| lower_value(value, ctx))
                .collect::<Result<_, _>>()?,
        ),

        ModelValue::Object(object) => JsonValue::Object(lower_object(object, ctx)?),

        ModelValue::Func { func, range } => lower_value(eval_func(func, range, ctx)?, ctx)?,

        ModelValue::Unary { op, operand, range } => {
            let operand = eval_operand(*operand, ctx)?;
            scalar_to_json(eval_unary(op, operand, range)?)
        }

        ModelValue::Binary { op, lhs, rhs, range } => scalar_to_json(eval_binary(op, *lhs, *rhs, range, ctx)?),

        // branches may be arbitrary values, only the taken one is evaluated
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool(ctx)? {
                then
            } else {
                otherwise
            };
            lower_value(*taken, ctx)?
        }

        ModelValue::Index { target, index, range } => lower_value(eval_index(*target, *index, range, ctx)?, ctx)?,

        ModelValue::Slice {
            target,
            start,
            end,
            range,
        } => lower_value(eval_slice(*target, start, end, range, ctx)?, ctx)?,
    };

    Ok(value)
}

// generous bounds clamp like python
fn eval_slice(
    target: ModelValue,
    start: Option<Box<ModelValue>>,
    end: Option<Box<ModelValue>>,
    range: SrcRange,
    ctx: &mut Ctx,
) -> Result<ModelValue, Error> {
    let ModelValue::Array(ModelArray { elem }) = eval_container(target, ctx)? else {
        unreachable!("modeler validated the slice target as an array");
    };
    let elem = splice_array(elem, ctx)?;

    let len = elem.len();
    let start = match start {
        Some(bound) => eval_slice_bound(*bound, range, ctx)?.min(len),
        None => 0,
    };
    let end = match end {
        Some(bound) => eval_slice_bound(*bound, range, ctx)?.min(len),
        None => len,
    };

    let elem = if start >= end {
        Vec::new()
    } else {
        elem.into_iter().skip(start).take(end - start).collect()
    };

    Ok(ModelValue::Array(ModelArray {
        elem: elem.into_iter().map(ModelArrayElem::Item).collect(),
    }))
}

fn eval_slice_bound(bound: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<usize, Error> {
    match eval_operand(bound, ctx)? {
        Scalar::Int(value) => usize::try_from(value).map_err(|_| Error::new(ErrorKind::NegativeSliceBound, range)),
        Scalar::Float(_) => Err(Error::new(ErrorKind::NonIntegerSliceBound, range)),
        _ => unreachable!("modeler validated slice bounds as numbers"),
    }
}

// siblings of the picked element stay unevaluated like an untaken branch
fn eval_index(target: ModelValue, index: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let target = eval_container(target, ctx)?;
    let index = eval_operand(index, ctx)?;

    match (target, index) {
        (ModelValue::Array(ModelArray { elem }), Scalar::Int(position)) => {
            let elem = splice_array(elem, ctx)?;
            usize::try_from(position)
                .ok()
                .filter(|&position| position < elem.len())
                .map(|position| elem.into_iter().nth(position).expect("position is in bounds"))
                .ok_or(Error::new(ErrorKind::IndexOutOfBounds, range))
        }
        (ModelValue::Array(_), Scalar::Float(_)) => Err(Error::new(ErrorKind::NonIntegerIndex, range)),
        (ModelValue::Object(ModelObject { elem }), Scalar::Str(key)) => elem
            .into_iter()
            .find(|field| field.key == key)
            .map(|field| field.value)
            .ok_or(Error::new(ErrorKind::MissingObjectKey, range)),
        _ => unreachable!("modeler validated index types"),
    }
}

fn eval_container(value: ModelValue, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    match value {
        ModelValue::Array(_) | ModelValue::Object(_) => Ok(value),
        ModelValue::VarRef(name) => eval_container(ctx.force_binding(&name)?, ctx),
        ModelValue::BlockRef { ref_id, range } => match json_to_value(ctx.resolve_ref(ref_id)?) {
            value @ (ModelValue::Array(_) | ModelValue::Object(_)) => Ok(value),
            _ => Err(Error::new(ErrorKind::RefShapeMismatch, range)),
        },
        ModelValue::Func { func, range } => eval_container(eval_func(func, range, ctx)?, ctx),
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool(ctx)? {
                then
            } else {
                otherwise
            };
            eval_container(*taken, ctx)
        }
        ModelValue::Index { target, index, range } => eval_container(eval_index(*target, *index, range, ctx)?, ctx),
        ModelValue::Slice {
            target,
            start,
            end,
            range,
        } => eval_slice(*target, start, end, range, ctx),
        _ => unreachable!("modeler validated the target as an array or object"),
    }
}

// a choice or weighted pick may itself still be dynamic, so callers recurse on the result
fn eval_func(func: ModelFunc, range: SrcRange, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let value = match func {
        ModelFunc::Choice(options) => {
            let pick = ctx.rng.random_range(0..options.len());
            options
                .into_iter()
                .nth(pick)
                .expect("modeler guarantees choice has an alternative")
        }

        ModelFunc::Weighted(options) => {
            let total: f64 = options.iter().map(|option| option.weight).sum();
            let draw = ctx.rng.random_range(0.0..total);
            let mut acc = 0.0;
            let mut pick = None;
            let mut fallback = None;
            for (index, option) in options.iter().enumerate() {
                if option.weight > 0.0 {
                    fallback = Some(index);
                }
                acc += option.weight;
                if pick.is_none() && draw < acc && option.weight > 0.0 {
                    pick = Some(index);
                }
            }
            // float accumulation can leave the draw past every bucket, fall back
            // to the last pickable option
            let pick = pick.or(fallback).expect("modeler guarantees a positive weight");
            options.into_iter().nth(pick).expect("pick indexes into the options").value
        }

        ModelFunc::Range(ModelRange::Int { min, max }) => ModelValue::Num(ModelNumber::Int(ctx.rng.random_range(min..=max))),
        ModelFunc::Range(ModelRange::Float { min, max }) => {
            ModelValue::Num(ModelNumber::Float(ctx.rng.random_range(min..=max)))
        }

        ModelFunc::Normal { mean, stddev } => {
            let sample = ctx.rng.sample(Normal::new(mean, stddev).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Lognormal { median, sigma } => {
            let sample = ctx
                .rng
                .sample(LogNormal::new(median.ln(), sigma).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Exponential { mean } => {
            let sample = ctx.rng.sample(Exp::new(1.0 / mean).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Pareto { min, shape } => {
            let sample = ctx.rng.sample(Pareto::new(min, shape).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Beta { alpha, beta } => {
            let sample = ctx.rng.sample(Beta::new(alpha, beta).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Poisson { mean } => {
            let sample: f64 = ctx.rng.sample(Poisson::new(mean).expect("modeler validated params"));
            ModelValue::Num(ModelNumber::Int(sample as i64))
        }
        ModelFunc::Chance { probability } => ModelValue::Bool(ctx.rng.random_bool(probability)),

        ModelFunc::Upper { text } => ModelValue::Str(eval_text(*text, ctx)?.to_uppercase()),
        ModelFunc::Lower { text } => ModelValue::Str(eval_text(*text, ctx)?.to_lowercase()),
        ModelFunc::Trim { text } => ModelValue::Str(eval_text(*text, ctx)?.trim().to_owned()),
        ModelFunc::Replace { text, from, to } => {
            let text = eval_text(*text, ctx)?;
            let from = eval_text(*from, ctx)?;
            let to = eval_text(*to, ctx)?;
            // replacing an empty pattern would splice `to` between every character
            ModelValue::Str(if from.is_empty() { text } else { text.replace(&from, &to) })
        }
        ModelFunc::Split { text, separator } => {
            let text = eval_text(*text, ctx)?;
            let separator = eval_text(*separator, ctx)?;
            if separator.is_empty() {
                return Err(Error::new(ErrorKind::EmptySplitSeparator, range));
            }
            ModelValue::Array(ModelArray {
                elem: text
                    .split(&separator)
                    .map(|part| ModelArrayElem::Item(ModelValue::Str(part.to_owned())))
                    .collect(),
            })
        }
        ModelFunc::Join { array, separator } => {
            let JsonValue::Array(elems) = lower_value(*array, ctx)? else {
                unreachable!("modeler validated the argument as an array");
            };
            let separator = eval_text(*separator, ctx)?;
            let parts = elems
                .iter()
                .map(|elem| json_text(elem).ok_or(Error::new(ErrorKind::JoinElementNotScalar, range)))
                .collect::<Result<Vec<_>, _>>()?;
            ModelValue::Str(parts.join(&separator))
        }
        ModelFunc::Contains { target, needle } => match lower_value(*target, ctx)? {
            JsonValue::String(text) => {
                let needle = eval_text(*needle, ctx)?;
                ModelValue::Bool(text.contains(&needle))
            }
            JsonValue::Array(elems) => {
                let needle = eval_operand(*needle, ctx)?;
                ModelValue::Bool(elems.iter().any(|elem| json_matches_scalar(elem, &needle)))
            }
            _ => unreachable!("modeler validated the target as a string or array"),
        },
        ModelFunc::StartsWith { text, prefix } => {
            let text = eval_text(*text, ctx)?;
            let prefix = eval_text(*prefix, ctx)?;
            ModelValue::Bool(text.starts_with(&prefix))
        }
        ModelFunc::EndsWith { text, suffix } => {
            let text = eval_text(*text, ctx)?;
            let suffix = eval_text(*suffix, ctx)?;
            ModelValue::Bool(text.ends_with(&suffix))
        }
        ModelFunc::Len { target } => {
            let length = match lower_value(*target, ctx)? {
                JsonValue::String(text) => text.chars().count(),
                JsonValue::Array(elems) => elems.len(),
                // reference-fed targets settle their types here
                _ => return Err(shape_error(ctx)),
            };
            ModelValue::Num(ModelNumber::Int(length as i64))
        }
        ModelFunc::Tokens { value } => {
            // containers count their compact json serialization
            let text = match lower_value(*value, ctx)? {
                JsonValue::String(text) => text,
                value => value.to_string(),
            };
            ModelValue::Num(ModelNumber::Int(BPE.encode_ordinary(&text).len() as i64))
        }
        ModelFunc::Format { pieces, args } => {
            // the modeler guarantees one more piece than args
            let mut pieces = pieces.into_iter();
            let mut text = pieces.next().unwrap_or_default();
            for (arg, piece) in args.into_iter().zip(pieces) {
                text.push_str(&scalar_text(eval_operand(arg, ctx)?));
                text.push_str(&piece);
            }
            ModelValue::Str(text)
        }

        ModelFunc::Clamp { value, min, max } => {
            let value = eval_operand(*value, ctx)?;
            let min = eval_operand(*min, ctx)?;
            let max = eval_operand(*max, ctx)?;
            match (value, min, max) {
                (Scalar::Int(value), Scalar::Int(min), Scalar::Int(max)) => {
                    if min > max {
                        return Err(Error::new(ErrorKind::ClampBoundsOutOfOrder, range));
                    }
                    ModelValue::Num(ModelNumber::Int(value.clamp(min, max)))
                }
                (value, min, max) => {
                    let (value, min, max) = (value.as_float(), min.as_float(), max.as_float());
                    if min > max {
                        return Err(Error::new(ErrorKind::ClampBoundsOutOfOrder, range));
                    }
                    ModelValue::Num(ModelNumber::Float(value.clamp(min, max)))
                }
            }
        }
        ModelFunc::Round { value } => eval_to_int(eval_operand(*value, ctx)?, f64::round, range)?,
        ModelFunc::Floor { value } => eval_to_int(eval_operand(*value, ctx)?, f64::floor, range)?,
        ModelFunc::Ceil { value } => eval_to_int(eval_operand(*value, ctx)?, f64::ceil, range)?,
        ModelFunc::Abs { value } => match eval_operand(*value, ctx)? {
            Scalar::Int(value) => ModelValue::Num(ModelNumber::Int(
                value.checked_abs().ok_or(Error::new(ErrorKind::NonFiniteResult, range))?,
            )),
            Scalar::Float(value) => ModelValue::Num(ModelNumber::Float(value.abs())),
            _ => unreachable!("modeler validated the argument as a number"),
        },
        ModelFunc::Min(values) => eval_extreme(values, true, ctx)?,
        ModelFunc::Max(values) => eval_extreme(values, false, ctx)?,

        ModelFunc::Uuid => {
            let bytes: [u8; 16] = ctx.rng.random();
            ModelValue::Str(uuid::Builder::from_random_bytes(bytes).into_uuid().to_string())
        }
        ModelFunc::Hex { length } => ModelValue::Str(random_text(b"0123456789abcdef", length, ctx)),
        ModelFunc::Alphanum { length } => ModelValue::Str(random_text(
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            length,
            ctx,
        )),
    };

    Ok(value)
}

fn finite_float(value: f64, range: SrcRange) -> Result<ModelValue, Error> {
    if value.is_finite() {
        Ok(ModelValue::Num(ModelNumber::Float(value)))
    } else {
        Err(Error::new(ErrorKind::NonFiniteResult, range))
    }
}

fn eval_text(value: ModelValue, ctx: &mut Ctx) -> Result<String, Error> {
    match eval_operand(value, ctx)? {
        Scalar::Str(text) => Ok(text),
        // reference-fed arguments settle their types here
        _ => Err(shape_error(ctx)),
    }
}

fn eval_to_int(scalar: Scalar, op: fn(f64) -> f64, range: SrcRange) -> Result<ModelValue, Error> {
    let value = match scalar {
        Scalar::Int(value) => value,
        Scalar::Float(value) => {
            let value = op(value);
            if value < i64::MIN as f64 || value > i64::MAX as f64 {
                return Err(Error::new(ErrorKind::NonFiniteResult, range));
            }
            value as i64
        }
        // reference-fed arguments settle their types here
        _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
    };

    Ok(ModelValue::Num(ModelNumber::Int(value)))
}

fn eval_extreme(values: Vec<ModelValue>, minimize: bool, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let scalars = values
        .into_iter()
        .map(|value| eval_operand(value, ctx))
        .collect::<Result<Vec<_>, _>>()?;

    let number = if scalars.iter().any(|scalar| matches!(scalar, Scalar::Float(_))) {
        let floats = scalars.iter().map(Scalar::as_float);
        ModelNumber::Float(if minimize {
            floats.fold(f64::INFINITY, f64::min)
        } else {
            floats.fold(f64::NEG_INFINITY, f64::max)
        })
    } else {
        let ints = scalars.iter().map(|scalar| match scalar {
            Scalar::Int(value) => *value,
            _ => unreachable!("modeler validated the arguments as numbers"),
        });
        ModelNumber::Int(if minimize {
            ints.min().expect("modeler requires at least two arguments")
        } else {
            ints.max().expect("modeler requires at least two arguments")
        })
    };

    Ok(ModelValue::Num(number))
}

fn random_text(charset: &[u8], length: usize, ctx: &mut Ctx) -> String {
    (0..length)
        .map(|_| charset[ctx.rng.random_range(0..charset.len())] as char)
        .collect()
}

fn json_text(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Number(number) => Some(number.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_text(scalar: Scalar) -> String {
    match scalar {
        Scalar::Str(text) => text,
        Scalar::Int(value) => value.to_string(),
        // {:?} keeps a .0 so floats stay recognizable
        Scalar::Float(value) => format!("{value:?}"),
        Scalar::Bool(value) => value.to_string(),
    }
}

// equality between an evaluated element and the needle, ints and floats unify
fn json_matches_scalar(value: &JsonValue, needle: &Scalar) -> bool {
    match (value, needle) {
        (JsonValue::String(value), Scalar::Str(needle)) => value == needle,
        (JsonValue::Bool(value), Scalar::Bool(needle)) => value == needle,
        (JsonValue::Number(value), Scalar::Int(needle)) => match value.as_i64() {
            Some(value) => value == *needle,
            None => value.as_f64() == Some(*needle as f64),
        },
        (JsonValue::Number(value), Scalar::Float(needle)) => value.as_f64() == Some(*needle),
        _ => false,
    }
}

fn lower_object(object: ModelObject, ctx: &mut Ctx) -> Result<JsonMap<String, JsonValue>, Error> {
    object
        .elem
        .into_iter()
        .map(|ModelObjectField { key, value }| Ok((key, lower_value(value, ctx)?)))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Scalar {
    // reference-fed values settle their types here, not in the modeler
    fn into_bool(self, ctx: &Ctx) -> Result<bool, Error> {
        match self {
            Self::Bool(value) => Ok(value),
            _ => Err(shape_error(ctx)),
        }
    }

    fn as_float(&self) -> f64 {
        match self {
            Self::Int(value) => *value as f64,
            Self::Float(value) => *value,
            _ => unreachable!("modeler validated operand types"),
        }
    }
}

// finiteness is checked by eval before conversion
fn scalar_to_json(scalar: Scalar) -> JsonValue {
    match scalar {
        Scalar::Int(value) => JsonValue::Number(value.into()),
        Scalar::Float(value) => JsonValue::Number(JsonNumber::from_f64(value).expect("evaluation rejects non-finite results")),
        Scalar::Bool(value) => JsonValue::Bool(value),
        Scalar::Str(value) => JsonValue::String(value),
    }
}

fn eval_operand(value: ModelValue, ctx: &mut Ctx) -> Result<Scalar, Error> {
    let scalar = match value {
        ModelValue::Str(value) => Scalar::Str(value),
        ModelValue::Template(template) => Scalar::Str(resolve_template(template, ctx)?),
        ModelValue::Num(ModelNumber::Int(value)) => Scalar::Int(value),
        ModelValue::Num(ModelNumber::Float(value)) => Scalar::Float(value),
        ModelValue::Bool(value) => Scalar::Bool(value),

        ModelValue::VarRef(name) => eval_operand(ctx.force_binding(&name)?, ctx)?,
        ModelValue::CtxRef(ctx_ref) => Scalar::Int(ctx.ctx_value(ctx_ref)),
        ModelValue::BlockRef { ref_id, range } => match ctx.resolve_ref(ref_id)? {
            JsonValue::String(value) => Scalar::Str(value),
            JsonValue::Bool(value) => Scalar::Bool(value),
            JsonValue::Number(number) => match number.as_i64() {
                Some(value) => Scalar::Int(value),
                None => Scalar::Float(number.as_f64().expect("json numbers convert to floats")),
            },
            _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
        },
        ModelValue::Func { func, range } => eval_operand(eval_func(func, range, ctx)?, ctx)?,

        ModelValue::Unary { op, operand, range } => {
            let operand = eval_operand(*operand, ctx)?;
            eval_unary(op, operand, range)?
        }
        ModelValue::Binary { op, lhs, rhs, range } => eval_binary(op, *lhs, *rhs, range, ctx)?,
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool(ctx)? {
                then
            } else {
                otherwise
            };
            eval_operand(*taken, ctx)?
        }
        ModelValue::Index { target, index, range } => eval_operand(eval_index(*target, *index, range, ctx)?, ctx)?,

        // reference-fed values can put any json shape here, so what the
        // modeler used to guarantee statically is checked at evaluation
        ModelValue::Null | ModelValue::Array(_) | ModelValue::Object(_) => return Err(shape_error(ctx)),
        // a residual slice is still rejected statically, refs never make one
        ModelValue::Slice { .. } => unreachable!("modeler validated operand types"),
    };

    Ok(scalar)
}

// the culprit for a shape mismatch is the last reference crossed; without one
// the value came from a modeler-validated position and the range is unknown
fn shape_error(ctx: &Ctx) -> Error {
    Error::new(ErrorKind::RefShapeMismatch, ctx.last_ref.unwrap_or(SrcRange::new(0, 0)))
}

fn eval_unary(op: UnaryOp, operand: Scalar, range: SrcRange) -> Result<Scalar, Error> {
    let scalar = match (op, operand) {
        (UnaryOp::Neg, Scalar::Int(value)) => {
            Scalar::Int(value.checked_neg().ok_or(Error::new(ErrorKind::NonFiniteResult, range))?)
        }
        (UnaryOp::Neg, Scalar::Float(value)) => Scalar::Float(-value),
        (UnaryOp::Not, Scalar::Bool(value)) => Scalar::Bool(!value),
        _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
    };

    Ok(scalar)
}

fn eval_binary(op: BinOp, lhs: ModelValue, rhs: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<Scalar, Error> {
    // logical ops short-circuit so guard idioms never evaluate the right side
    if matches!(op, BinOp::And | BinOp::Or) {
        let left = eval_operand(lhs, ctx)?.into_bool(ctx)?;
        let value = match (op, left) {
            (BinOp::And, false) => false,
            (BinOp::Or, true) => true,
            _ => eval_operand(rhs, ctx)?.into_bool(ctx)?,
        };
        return Ok(Scalar::Bool(value));
    }

    let numeric = |scalar: &Scalar| matches!(scalar, Scalar::Int(_) | Scalar::Float(_));

    // equality tolerates null so absence guards read naturally
    if matches!(op, BinOp::Eq | BinOp::Ne) {
        let lhs = eval_nullable(lhs, ctx)?;
        let rhs = eval_nullable(rhs, ctx)?;
        let equal = match (&lhs, &rhs) {
            (None, None) => true,
            (None, Some(_)) | (Some(_), None) => false,
            (Some(lhs), Some(rhs)) => match (lhs, rhs) {
                (Scalar::Str(lhs), Scalar::Str(rhs)) => lhs == rhs,
                (Scalar::Bool(lhs), Scalar::Bool(rhs)) => lhs == rhs,
                (Scalar::Int(lhs), Scalar::Int(rhs)) => lhs == rhs,
                _ if numeric(lhs) && numeric(rhs) => lhs.as_float() == rhs.as_float(),
                _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
            },
        };
        return Ok(Scalar::Bool(if op == BinOp::Eq { equal } else { !equal }));
    }

    let lhs = eval_operand(lhs, ctx)?;
    let rhs = eval_operand(rhs, ctx)?;

    let scalar = match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            if !numeric(&lhs) || !numeric(&rhs) {
                return Err(Error::new(ErrorKind::RefShapeMismatch, range));
            }
            eval_arith(op, lhs, rhs, range)?
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ordering = match (&lhs, &rhs) {
                (Scalar::Int(lhs), Scalar::Int(rhs)) => lhs.partial_cmp(rhs),
                _ if numeric(&lhs) && numeric(&rhs) => lhs.as_float().partial_cmp(&rhs.as_float()),
                _ => return Err(Error::new(ErrorKind::RefShapeMismatch, range)),
            };
            let ordering = ordering.expect("finite numbers are always ordered");
            Scalar::Bool(match op {
                BinOp::Lt => ordering.is_lt(),
                BinOp::Le => ordering.is_le(),
                BinOp::Gt => ordering.is_gt(),
                BinOp::Ge => ordering.is_ge(),
                _ => unreachable!("operator is a comparison"),
            })
        }
        BinOp::Eq | BinOp::Ne => unreachable!("equality evaluates through the nullable path above"),
        BinOp::And | BinOp::Or => unreachable!("logical operators short-circuit above"),
    };

    Ok(scalar)
}

// a scalar or an explicit absence, for equality's null probes
fn eval_nullable(value: ModelValue, ctx: &mut Ctx) -> Result<Option<Scalar>, Error> {
    match value {
        ModelValue::Null => Ok(None),
        ModelValue::BlockRef { ref_id, range } => match ctx.resolve_ref(ref_id)? {
            JsonValue::Null => Ok(None),
            JsonValue::String(value) => Ok(Some(Scalar::Str(value))),
            JsonValue::Bool(value) => Ok(Some(Scalar::Bool(value))),
            JsonValue::Number(number) => Ok(Some(match number.as_i64() {
                Some(value) => Scalar::Int(value),
                None => Scalar::Float(number.as_f64().expect("json numbers convert to floats")),
            })),
            _ => Err(Error::new(ErrorKind::RefShapeMismatch, range)),
        },
        ModelValue::VarRef(name) => {
            let value = ctx.force_binding(&name)?;
            eval_nullable(value, ctx)
        }
        ModelValue::Index { target, index, range } => {
            let value = eval_index(*target, *index, range, ctx)?;
            eval_nullable(value, ctx)
        }
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool(ctx)? {
                then
            } else {
                otherwise
            };
            eval_nullable(*taken, ctx)
        }
        ModelValue::Func { func, range } => {
            let value = eval_func(func, range, ctx)?;
            eval_nullable(value, ctx)
        }
        value => Ok(Some(eval_operand(value, ctx)?)),
    }
}

fn eval_arith(op: BinOp, lhs: Scalar, rhs: Scalar, range: SrcRange) -> Result<Scalar, Error> {
    let scalar = match (lhs, rhs) {
        (Scalar::Int(lhs), Scalar::Int(rhs)) => {
            if matches!(op, BinOp::Div | BinOp::Rem) && rhs == 0 {
                return Err(Error::new(ErrorKind::DivisionByZero, range));
            }
            let result = match op {
                BinOp::Add => lhs.checked_add(rhs),
                BinOp::Sub => lhs.checked_sub(rhs),
                BinOp::Mul => lhs.checked_mul(rhs),
                // checked catches i64::MIN / -1
                BinOp::Div => lhs.checked_div(rhs),
                BinOp::Rem => lhs.checked_rem(rhs),
                _ => unreachable!("operator is arithmetic"),
            };
            Scalar::Int(result.ok_or(Error::new(ErrorKind::NonFiniteResult, range))?)
        }
        (lhs, rhs) => {
            let (lhs, rhs) = (lhs.as_float(), rhs.as_float());
            if matches!(op, BinOp::Div | BinOp::Rem) && rhs == 0.0 {
                return Err(Error::new(ErrorKind::DivisionByZero, range));
            }
            let result = match op {
                BinOp::Add => lhs + rhs,
                BinOp::Sub => lhs - rhs,
                BinOp::Mul => lhs * rhs,
                BinOp::Div => lhs / rhs,
                BinOp::Rem => lhs % rhs,
                _ => unreachable!("operator is arithmetic"),
            };
            if !result.is_finite() {
                return Err(Error::new(ErrorKind::NonFiniteResult, range));
            }
            Scalar::Float(result)
        }
    };

    Ok(scalar)
}

fn resolve_template(template: ModelTemplate, ctx: &mut Ctx) -> Result<String, Error> {
    let mut text = String::new();
    for part in template.parts {
        match part {
            ModelPart::Lit(value) => text.push_str(&value),
            ModelPart::Ref(ctx_ref) => text.push_str(&ctx.ctx_value(ctx_ref).to_string()),
            ModelPart::VarRef(name) => match ctx.force_binding(&name)? {
                ModelValue::Str(value) => text.push_str(&value),
                ModelValue::Num(ModelNumber::Int(value)) => text.push_str(&value.to_string()),
                ModelValue::Num(ModelNumber::Float(value)) => text.push_str(&scalar_text(Scalar::Float(value))),
                ModelValue::Bool(value) => text.push_str(if value { "true" } else { "false" }),
                _ => unreachable!("modeler validated interpolated bindings as scalars"),
            },
            // scalar-ness the modeler deferred is enforced right here
            ModelPart::Dynamic(value) => {
                let scalar = eval_operand(value, ctx)?;
                text.push_str(&scalar_text(scalar));
            }
        }
    }
    Ok(text)
}

// multiple trace templates cycle in source order
pub(super) fn plan(model: Model, count: usize, seed: u64) -> Result<Plan, Error> {
    debug_assert!(!model.traces.is_empty());

    let capacity = (0..count)
        .map(|index| trace_len(&model.traces[index % model.traces.len()]))
        .sum();

    let mut planner = Planner {
        events: Vec::with_capacity(capacity),
        traces: Vec::with_capacity(count),
    };

    for index in 0..count {
        // every draw is addressed by seed, trace index, and instance path, so
        // a trace's values depend on nothing planned before it
        let mut ctx = Ctx::new(&model.refs, seed, index);
        ctx.instantiate_trace(&model.traces[index % model.traces.len()], &model.bindings)?;
        planner.emit_trace(&mut ctx)?;
    }

    Ok(Plan {
        events: planner.events.into_boxed_slice(),
        traces: planner.traces.into_boxed_slice(),
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct Error {
    kind: ErrorKind,
    pub(crate) range: SrcRange,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ErrorKind {
    DivisionByZero,
    NonFiniteResult,
    IndexOutOfBounds,
    NonIntegerIndex,
    MissingObjectKey,
    NonIntegerSliceBound,
    NegativeSliceBound,
    NegativeRepeatCount,
    NonIntegerRepeatCount,
    ChanceOutOfRange,
    ClampBoundsOutOfOrder,
    EmptySplitSeparator,
    JoinElementNotScalar,
    CircularReference,
    AbsentRefField,
    RefShapeMismatch,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DivisionByZero => "expression divides by zero",
            Self::NonFiniteResult => "expression result overflowed or is not finite",
            Self::IndexOutOfBounds => "array index is out of bounds",
            Self::NonIntegerIndex => "array index is not an integer",
            Self::MissingObjectKey => "object key is not present",
            Self::NonIntegerSliceBound => "slice bound is not an integer",
            Self::NegativeSliceBound => "slice bound is negative",
            Self::NegativeRepeatCount => "repeat count is negative",
            Self::NonIntegerRepeatCount => "repeat count is not an integer",
            Self::ChanceOutOfRange => "maybe chance is not between 0 and 1",
            Self::ClampBoundsOutOfOrder => "clamp bounds are out of order",
            Self::EmptySplitSeparator => "split separator is empty",
            Self::JoinElementNotScalar => "join element is not a string, number, or boolean",
            Self::CircularReference => "references form a cycle",
            Self::AbsentRefField => "referenced block does not set the field",
            Self::RefShapeMismatch => "referenced value has the wrong shape for this position",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;

    #[test]
    fn prints_plan() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        println!("{plan:#?}");
    }

    #[test]
    fn lowers_null_and_negative_numbers_to_json() {
        let model = compile(r#"trace "t" { input = null metrics = { delta = -0.5 } }"#).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let fields = &plan.events[0].fields;
        assert_eq!(fields.input, Some(JsonValue::Null));
        assert_eq!(fields.metrics.as_ref().unwrap()["delta"], JsonValue::from(-0.5));
    }

    #[test]
    fn resolves_trace_index_per_generated_trace() {
        let model = compile(r#"trace "t" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        let inputs = plan
            .events
            .iter()
            .map(|event| event.fields.input.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            [JsonValue::from("q 0"), JsonValue::from("q 1"), JsonValue::from("q 2")]
        );
        assert_eq!(plan.events[2].fields.tags.as_ref(), ["t-2"]);
    }

    #[test]
    fn evaluates_context_references_in_expressions() {
        let model = compile(
            r#"
            trace "t" {
                input = trace.index
                repeat {
                    count = 3
                    task "turn" { metrics = { turn = repeat.index + 1, of = repeat.count } }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 2, 0).unwrap();

        assert_eq!(plan.events[0].fields.input, Some(JsonValue::from(0)));
        assert_eq!(plan.events[4].fields.input, Some(JsonValue::from(1)));
        for (offset, event) in plan.events[1..4].iter().enumerate() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            assert_eq!(metrics["turn"], JsonValue::from(offset as i64 + 1));
            assert_eq!(metrics["of"], JsonValue::from(3));
        }
    }

    #[test]
    fn slices_trace_scope_vars_by_repeat_index() {
        let model = compile(
            r#"
            trace "t" {
                vars { messages = ["q0", "a0", "q1", "a1"] }
                repeat {
                    count = 2
                    task "turn" { input = var.messages[:(repeat.index * 2) + 1] }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        assert_eq!(plan.events[1].fields.input, Some(JsonValue::from(vec!["q0"])));
        assert_eq!(plan.events[2].fields.input, Some(JsonValue::from(vec!["q0", "a0", "q1"])));
    }

    #[test]
    fn resolves_repeat_refs_against_the_innermost_repeat() {
        let model = compile(
            r#"
            trace "t" {
                repeat {
                    count = 2
                    repeat {
                        count = repeat.index + 1
                        task "step" { metrics = { inner = repeat.index, of = repeat.count } }
                    }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        // outer iteration 0 runs the inner repeat once, iteration 1 twice
        let metrics = plan
            .events
            .iter()
            .skip(1)
            .map(|event| {
                let metrics = event.fields.metrics.as_ref().unwrap();
                (metrics["inner"].as_i64().unwrap(), metrics["of"].as_i64().unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(metrics, [(0, 1), (0, 2), (1, 2)]);
    }

    #[test]
    fn cycles_templates_to_the_requested_trace_count() {
        let model = compile(
            r#"
                trace "first" {}
                trace "second" {}
            "#,
        )
        .unwrap();
        let plan = plan(model, 5, 0).unwrap();

        assert_eq!(plan.traces.len(), 5);
        assert_eq!(
            plan.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
            ["first", "second", "first", "second", "first"]
        );
    }

    #[test]
    fn evaluates_choice_within_its_alternatives() {
        let model = compile(r#"trace "t" { input = choice("a", "b") }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b"), "unexpected choice {input}");
        }
    }

    #[test]
    fn evaluates_ranges_within_bounds_and_keeps_integer_bounds_integral() {
        let model = compile(r#"trace "t" { metrics = { tokens = range(80, 400), temp = range(0.0, 1.0) } }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            let tokens = metrics["tokens"].as_i64().unwrap();
            assert!((80..=400).contains(&tokens));
            let temp = metrics["temp"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&temp));
        }
    }

    #[test]
    fn evaluates_weighted_picks_proportionally_to_their_weights() {
        let model = compile(r#"trace "t" { input = weighted(["common", 9], ["rare", 1], ["never", 0]) }"#).unwrap();
        let plan = plan(model, 200, 7).unwrap();

        let mut counts = std::collections::HashMap::new();
        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            *counts.entry(input.to_owned()).or_insert(0) += 1;
        }
        assert!(!counts.contains_key("never"), "zero weights must never be picked");
        assert!(counts["common"] > counts["rare"], "weights must skew the distribution");
        assert!(counts["rare"] > 0, "positive weights must appear over 200 traces");
    }

    #[test]
    fn evaluates_weighted_picks_with_dynamic_values() {
        let model = compile(r#"trace "t" { input = weighted([range(1, 3), 1], [10, 1]) }"#).unwrap();
        let plan = plan(model, 40, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_i64().unwrap();
            assert!((1..=3).contains(&input) || input == 10, "unexpected pick {input}");
        }
    }

    #[test]
    fn evaluates_distributions_within_their_supports() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        normal = normal(100, 10)
                        lognormal = lognormal(300, 0.5)
                        exponential = exponential(250)
                        pareto = pareto(100, 1.5)
                        beta = beta(2, 5)
                        poisson = poisson(3)
                    }
                    input = chance(0.5)
                }
            "#,
        )
        .unwrap();
        let plan_a = plan(model, 50, 7).unwrap();

        for event in plan_a.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            assert!(metrics["lognormal"].as_f64().unwrap() > 0.0);
            assert!(metrics["exponential"].as_f64().unwrap() > 0.0);
            assert!(metrics["pareto"].as_f64().unwrap() >= 100.0);
            let beta = metrics["beta"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&beta));
            let poisson = metrics["poisson"].as_i64().unwrap();
            assert!(poisson >= 0, "poisson must be a non-negative integer");
            assert!(event.fields.input.as_ref().unwrap().is_boolean());
        }

        // samples vary across traces and reproduce by seed
        let normals: std::collections::HashSet<_> = plan_a
            .events
            .iter()
            .map(|event| event.fields.metrics.as_ref().unwrap()["normal"].to_string())
            .collect();
        assert!(normals.len() > 1, "expected varying samples over 50 traces");

        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        normal = normal(100, 10)
                        lognormal = lognormal(300, 0.5)
                        exponential = exponential(250)
                        pareto = pareto(100, 1.5)
                        beta = beta(2, 5)
                        poisson = poisson(3)
                    }
                    input = chance(0.5)
                }
            "#,
        )
        .unwrap();
        let plan_b = plan(model, 50, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_chance_bounds_deterministically() {
        let always = compile(r#"trace "t" { input = chance(1) }"#).unwrap();
        let plan_a = plan(always, 10, 7).unwrap();
        assert!(
            plan_a
                .events
                .iter()
                .all(|event| event.fields.input == Some(JsonValue::Bool(true)))
        );

        let never = compile(r#"trace "t" { input = chance(0) }"#).unwrap();
        let plan_b = plan(never, 10, 7).unwrap();
        assert!(
            plan_b
                .events
                .iter()
                .all(|event| event.fields.input == Some(JsonValue::Bool(false)))
        );
    }

    #[test]
    fn evaluates_string_funcs() {
        let model = compile(
            r#"
                vars { tags = ["alpha", "beta"] }
                trace "t" {
                    input = [
                        upper("get"),
                        lower("WARN"),
                        trim("  padded  "),
                        replace("a b c", " ", "-"),
                        replace("keep", "", "x"),
                        join(var.tags, ", "),
                        join([1, 2.5, true], "-"),
                        format("model={} n={}", "gpt", 4),
                    ]
                    output = split("a,b,c", ",")
                    metadata = {
                        sub = contains("gpt-4o-mini", "mini")
                        missing = contains("gpt-4o", "mini")
                        elem = contains(var.tags, "beta")
                        absent = contains(var.tags, "gamma")
                        prefix = starts_with("gpt-4o", "gpt")
                        suffix = ends_with("gpt-4o", "4o")
                        chars = len("hello")
                        elems = len(var.tags)
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let fields = &plan.events[0].fields;
        assert_eq!(
            fields.input,
            Some(serde_json::json!([
                "GET",
                "warn",
                "padded",
                "a-b-c",
                "keep",
                "alpha, beta",
                "1-2.5-true",
                "model=gpt n=4",
            ]))
        );
        assert_eq!(fields.output, Some(serde_json::json!(["a", "b", "c"])));
        assert_eq!(
            fields.metadata,
            Some(
                serde_json::json!({
                    "sub": true,
                    "missing": false,
                    "elem": true,
                    "absent": false,
                    "prefix": true,
                    "suffix": true,
                    "chars": 5,
                    "elems": 2,
                })
                .as_object()
                .unwrap()
                .clone()
            )
        );
    }

    #[test]
    fn evaluates_token_counts() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        text = tokens("What is the weather in Tokyo?")
                        empty = tokens("")
                        messages = tokens([{ role = "user", content = "hi" }])
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let metrics = plan.events[0].fields.metrics.as_ref().unwrap();
        assert_eq!(metrics["text"], JsonValue::from(7));
        assert_eq!(metrics["empty"], JsonValue::from(0));
        // containers count their compact json serialization
        let serialized = r#"[{"content":"hi","role":"user"}]"#;
        assert_eq!(metrics["messages"], JsonValue::from(BPE.encode_ordinary(serialized).len()));
    }

    #[test]
    fn evaluates_string_funcs_over_dynamic_args() {
        let model = compile(r#"trace "t" { input = upper(choice("get", "post")) }"#).unwrap();
        let planned = plan(model, 20, 7).unwrap();

        let mut seen = std::collections::HashSet::new();
        for event in planned.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "GET" | "POST"), "unexpected value {input}");
            seen.insert(input.to_owned());
        }
        assert_eq!(seen.len(), 2, "expected both alternatives over 20 traces");

        // split results behave as arrays for indexing and lens
        let model =
            compile(r#"trace "t" { input = split("a,b,c", ",")[range(0, 2)] output = len(split("a,b", ",")) }"#).unwrap();
        let planned = plan(model, 10, 7).unwrap();
        for event in planned.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b" | "c"));
            assert_eq!(event.fields.output, Some(JsonValue::from(2)));
        }
    }

    #[test]
    fn evaluates_format_with_dynamic_args() {
        let model = compile(r#"trace "t" { input = format("n={} f={} b={}", range(1, 1), 0.5, chance(1)) }"#).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        assert_eq!(plan.events[0].fields.input, Some(JsonValue::from("n=1 f=0.5 b=true")));
    }

    #[test]
    fn evaluates_numeric_funcs() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        low = clamp(1, 5, 10)
                        high = clamp(50, 5, 10)
                        inside = clamp(7, 5, 10)
                        promoted = clamp(7, 5.0, 10)
                        round = round(2.5)
                        down = floor(2.9)
                        up = ceil(2.1)
                        int = round(3)
                        abs = abs(0 - 4)
                        fabs = abs(0.0 - 4.5)
                        min = min(3, 1, 2)
                        max = max(3, 1, 2)
                        fmin = min(3, 1.5)
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let metrics = plan.events[0].fields.metrics.as_ref().unwrap();
        assert_eq!(metrics["low"], JsonValue::from(5));
        assert_eq!(metrics["high"], JsonValue::from(10));
        assert_eq!(metrics["inside"], JsonValue::from(7));
        assert_eq!(metrics["promoted"], JsonValue::from(7.0));
        assert_eq!(metrics["round"], JsonValue::from(3));
        assert_eq!(metrics["down"], JsonValue::from(2));
        assert_eq!(metrics["up"], JsonValue::from(3));
        assert_eq!(metrics["int"], JsonValue::from(3));
        assert_eq!(metrics["abs"], JsonValue::from(4));
        assert_eq!(metrics["fabs"], JsonValue::from(4.5));
        assert_eq!(metrics["min"], JsonValue::from(1));
        assert_eq!(metrics["max"], JsonValue::from(3));
        assert_eq!(metrics["fmin"], JsonValue::from(1.5));
    }

    #[test]
    fn clamps_distribution_samples_within_bounds() {
        let model = compile(r#"trace "t" { metrics = { latency = clamp(lognormal(300, 1.5), 20, 1000) } }"#).unwrap();
        let plan = plan(model, 50, 7).unwrap();

        for event in plan.events.iter() {
            let latency = event.fields.metrics.as_ref().unwrap()["latency"].as_f64().unwrap();
            assert!((20.0..=1000.0).contains(&latency), "unexpected latency {latency}");
        }
    }

    #[test]
    fn fails_on_invalid_dynamic_func_values() {
        let source = r#"trace "t" { input = clamp(1, range(5, 5), 2) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "clamp bounds are out of order");
        assert_eq!(&source[error.range.start..error.range.end], "clamp(1, range(5, 5), 2)");

        let source = r#"trace "t" { input = split("a,b", choice(",", "")) }"#;
        let error = plan(compile(source).unwrap(), 20, 7).unwrap_err();
        assert_eq!(error.to_string(), "split separator is empty");

        let source = r#"trace "t" { input = join([{}], ",") }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "join element is not a string, number, or boolean");

        let source = r#"trace "t" { input = round(99999999999999999999.0 * range(1.0, 1.0)) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");
    }

    #[test]
    fn evaluates_id_funcs_reproducibly() {
        let source = r#"trace "t" { input = [uuid(), hex(16), alphanum(12), hex(0)] }"#;
        let plan_a = plan(compile(source).unwrap(), 5, 7).unwrap();

        let mut uuids = std::collections::HashSet::new();
        for event in plan_a.events.iter() {
            let JsonValue::Array(values) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };

            let uuid = values[0].as_str().unwrap();
            assert_eq!(uuid.len(), 36);
            assert_eq!(uuid.as_bytes()[14], b'4', "expected a version-4 uuid");
            uuids.insert(uuid.to_owned());

            let hex = values[1].as_str().unwrap();
            assert_eq!(hex.len(), 16);
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

            let alphanum = values[2].as_str().unwrap();
            assert_eq!(alphanum.len(), 12);
            assert!(alphanum.chars().all(|c| c.is_ascii_alphanumeric()));

            assert_eq!(values[3].as_str().unwrap(), "");
        }
        assert_eq!(uuids.len(), 5, "each trace draws its own uuid");

        let plan_b = plan(compile(source).unwrap(), 5, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_nested_funcs_and_funcs_from_vars() {
        let model = compile(
            r#"
                vars { style = choice("clear", "vague") }
                trace "t" { input = [choice(range(1, 3), "x"), var.style] }
            "#,
        )
        .unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let JsonValue::Array(input) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };
            match &input[0] {
                JsonValue::Number(number) => assert!((1..=3).contains(&number.as_i64().unwrap())),
                JsonValue::String(value) => assert_eq!(value, "x"),
                other => panic!("unexpected value {other}"),
            }
            assert!(matches!(input[1].as_str().unwrap(), "clear" | "vague"));
        }
    }

    #[test]
    fn reproduces_the_same_plan_for_the_same_seed() {
        let source = r#"trace "t" { input = choice("a", "b", "c") metrics = { n = range(0, 1000000) } }"#;
        let first = plan(compile(source).unwrap(), 10, 42).unwrap();
        let second = plan(compile(source).unwrap(), 10, 42).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn samples_traces_independently_of_earlier_traces() {
        // trace 3 of a 5-trace run must match trace 3 of a 4-trace run
        let source = r#"trace "t" { metrics = { n = range(0, 1000000) } }"#;
        let longer = plan(compile(source).unwrap(), 5, 42).unwrap();
        let shorter = plan(compile(source).unwrap(), 4, 42).unwrap();

        assert_eq!(longer.events[3], shorter.events[3]);
    }

    #[test]
    fn evaluates_dynamic_arithmetic_within_bounds() {
        let source = r#"trace "t" { metrics = { n = range(1, 5) * 100, m = -range(1, 5) } }"#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        for event in plan_a.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            let n = metrics["n"].as_i64().unwrap();
            assert!((100..=500).contains(&n) && n % 100 == 0, "unexpected n {n}");
            let m = metrics["m"].as_i64().unwrap();
            assert!((-5..=-1).contains(&m), "unexpected m {m}");
        }

        // same seed, same values
        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_dynamic_ternaries_per_trace() {
        let model = compile(r#"trace "t" { input = choice(true, false) ? "a" : "b" }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        let mut seen = std::collections::HashSet::new();
        for event in plan.events.iter() {
            seen.insert(event.fields.input.as_ref().unwrap().as_str().unwrap().to_owned());
        }
        assert_eq!(seen.len(), 2, "expected both branches over 20 traces");
    }

    #[test]
    fn compares_resolved_templates_per_trace() {
        let model = compile(r#"trace "t" { input = "i ${trace.index}" == "i 1" ? 1 : 0 }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        let inputs: Vec<_> = plan
            .events
            .iter()
            .map(|event| event.fields.input.as_ref().unwrap().as_i64().unwrap())
            .collect();
        assert_eq!(inputs, [0, 1, 0]);
    }

    #[test]
    fn short_circuits_guarded_divisions() {
        // the zero divisor is never evaluated, so neither shape fails
        let model = compile(r#"trace "t" { input = range(0, 0) == 0 ? 0 : 100 / range(0, 0) }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();
        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(0)));

        let model = compile(r#"trace "t" { input = range(0, 0) != 0 && 100 / range(0, 0) > 1 }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();
        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(false)));
    }

    #[test]
    fn evaluates_dynamic_indexes_within_their_containers() {
        let source = r#"
            vars { user = { a = 1, b = 2 } }
            trace "t" {
                input = ["a", "b", "c"][range(0, 2)]
                output = var.user[choice("a", "b")]
            }
        "#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut inputs = std::collections::HashSet::new();
        for event in plan_a.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b" | "c"), "unexpected element {input}");
            inputs.insert(input.to_owned());
            let output = event.fields.output.as_ref().unwrap().as_i64().unwrap();
            assert!(matches!(output, 1 | 2), "unexpected value {output}");
        }
        assert!(inputs.len() > 1, "expected multiple elements over 20 traces");

        // same seed, same selections
        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_nested_dynamic_indexes() {
        let source = r#"
            vars { matrix = [[1, 2], [3, 4]] }
            trace "t" { input = var.matrix[range(0, 1)][range(0, 1)] }
        "#;
        let plan = plan(compile(source).unwrap(), 20, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_i64().unwrap();
            assert!((1..=4).contains(&input), "unexpected element {input}");
        }
    }

    #[test]
    fn evaluates_only_the_selected_element() {
        // the division by zero sits in an unselected element, so nothing fails
        let model = compile(r#"trace "t" { input = [0, 100 / range(0, 0)][range(0, 0)] }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();

        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(0)));
    }

    #[test]
    fn fails_on_invalid_dynamic_indexes() {
        let source = r#"trace "t" { input = [1][range(1, 1)] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "array index is out of bounds");
        assert_eq!(&source[error.range.start..error.range.end], "[1][range(1, 1)]");

        let source = r#"trace "t" { input = [1, 2][range(0.0, 0.0)] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "array index is not an integer");

        let source = r#"trace "t" { input = { a = 1 }[choice("b")] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "object key is not present");
    }

    #[test]
    fn evaluates_dynamic_slices_with_clamped_bounds() {
        let source = r#"
            trace "t" {
                input = [1, 2, 3, 4][range(1, 1):range(3, 3)]
                output = [1, 2][0:range(5, 5)]
                metadata = { empty = [1, 2][range(1, 1):0] }
            }
        "#;
        let plan_a = plan(compile(source).unwrap(), 3, 7).unwrap();

        for event in plan_a.events.iter() {
            assert_eq!(event.fields.input, Some(serde_json::json!([2, 3])));
            assert_eq!(event.fields.output, Some(serde_json::json!([1, 2])));
            assert_eq!(event.fields.metadata.as_ref().unwrap()["empty"], serde_json::json!([]));
        }

        // same seed, same selections
        let plan_b = plan(compile(source).unwrap(), 3, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_indexes_into_sliced_targets() {
        let model = compile(r#"trace "t" { input = [1, 2, 3][range(1, 1):][0] }"#).unwrap();
        let planned = plan(model, 1, 0).unwrap();

        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(2)));
    }

    #[test]
    fn fails_on_invalid_dynamic_slice_bounds() {
        let source = r#"trace "t" { input = [1, 2][0 - range(1, 1):] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "slice bound is negative");
        assert_eq!(&source[error.range.start..error.range.end], "[1, 2][0 - range(1, 1):]");

        let source = r#"trace "t" { input = [1, 2][range(0.5, 0.5):] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "slice bound is not an integer");
    }

    #[test]
    fn evaluates_unrolled_for_bodies_per_trace() {
        let model = compile(r#"trace "t" { input = [for x in [1, 2] : x * range(1, 3)] }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let JsonValue::Array(input) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };
            assert!((1..=3).contains(&input[0].as_i64().unwrap()));
            assert!((2..=6).contains(&input[1].as_i64().unwrap()));
        }
    }

    #[test]
    fn fails_on_dynamic_division_by_zero() {
        let source = r#"trace "t" { input = 100 / range(0, 0) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();

        assert_eq!(error.to_string(), "expression divides by zero");
        assert_eq!(&source[error.range.start..error.range.end], "100 / range(0, 0)");
    }

    #[test]
    fn fails_on_dynamic_overflow_and_non_finite_results() {
        let source = r#"trace "t" { metrics = { n = 9223372036854775807 + range(1, 1) } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");

        // ~9.99e307 is finite on its own, doubling it overflows f64
        let big = format!("{}.0", "9".repeat(308));
        let source = format!(r#"trace "t" {{ input = {big} * range(2.0, 2.0) }}"#);
        let error = plan(compile(&source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");
    }

    #[test]
    fn repeats_children_a_constant_number_of_times() {
        let model = compile(r#"trace "t" { repeat { count = 3 task "turn" { llm "reply" {} } } }"#).unwrap();
        let plan = plan(model, 2, 0).unwrap();

        // per trace: root + 3 * (task + llm)
        assert_eq!(plan.events.len(), 14);
        for range in plan.traces.iter() {
            let root = EventRef(range.start);
            let events = &plan.events[range.clone()];
            assert_eq!(events.len(), 7);
            assert_eq!(events.iter().filter(|event| event.kind == EventKind::Llm).count(), 3);
            assert!(events.iter().skip(1).all(|event| event.root == root));
        }
    }

    #[test]
    fn skips_repeats_with_a_zero_count() {
        let model = compile(r#"trace "t" { repeat { count = 0 task "turn" {} } }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        assert_eq!(plan.events.len(), 3);
    }

    #[test]
    fn varies_repeat_counts_per_trace_and_reproduces_them_by_seed() {
        let source = r#"trace "t" { repeat { count = range(1, 4) task "turn" {} } }"#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut lengths = std::collections::HashSet::new();
        for range in plan_a.traces.iter() {
            let turns = range.len() - 1;
            assert!((1..=4).contains(&turns), "unexpected turn count {turns}");
            lengths.insert(turns);
        }
        assert!(lengths.len() > 1, "expected varying trace shapes over 20 traces");

        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn resolves_repeat_index_per_iteration_with_the_innermost_repeat_winning() {
        let source = r#"
            trace "t" {
                repeat {
                    count = 2
                    task "outer" {
                        input = "o ${repeat.index}"
                        repeat {
                            count = 2
                            task "inner" { input = "i ${repeat.index}" }
                        }
                    }
                }
            }
        "#;
        let plan = plan(compile(source).unwrap(), 1, 0).unwrap();

        let inputs: Vec<_> = plan
            .events
            .iter()
            .filter_map(|event| event.fields.input.as_ref().and_then(|input| input.as_str()))
            .collect();
        assert_eq!(inputs, ["o 0", "i 0", "i 1", "o 1", "i 0", "i 1"]);
    }

    #[test]
    fn plans_exactly_one_choice_child() {
        let source = r#"trace "t" { choice { tool "a" {} function "b" {} } }"#;
        let plan = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut picked = std::collections::HashSet::new();
        for range in plan.traces.iter() {
            let events = &plan.events[range.clone()];
            assert_eq!(events.len(), 2, "choice must plan exactly one child");
            picked.insert(events[1].name.clone());
        }
        assert_eq!(picked.len(), 2, "expected both alternatives over 20 traces");
    }

    #[test]
    fn includes_maybe_children_probabilistically() {
        let source = r#"trace "t" { maybe { task "extra" {} } }"#;
        let plan_a = plan(compile(source).unwrap(), 40, 7).unwrap();

        let lengths: Vec<_> = plan_a.traces.iter().map(std::ops::Range::len).collect();
        assert!(
            lengths.contains(&1) && lengths.contains(&2),
            "expected both outcomes over 40 traces"
        );

        // the bounds always include or always skip
        let always = plan(
            compile(r#"trace "t" { maybe { chance = 1 task "extra" {} } }"#).unwrap(),
            10,
            7,
        )
        .unwrap();
        assert!(always.traces.iter().all(|range| range.len() == 2));
        let never = plan(
            compile(r#"trace "t" { maybe { chance = 0 task "extra" {} } }"#).unwrap(),
            10,
            7,
        )
        .unwrap();
        assert!(never.traces.iter().all(|range| range.len() == 1));
    }

    #[test]
    fn redraws_nested_dynamic_blocks_per_iteration() {
        let source = r#"
            trace "t" {
                repeat {
                    count = 8
                    choice { tool "a" {} function "b" {} }
                }
            }
        "#;
        let plan = plan(compile(source).unwrap(), 5, 7).unwrap();

        let names: std::collections::HashSet<_> = plan
            .events
            .iter()
            .filter(|event| event.parent.is_some())
            .map(|event| event.name.clone())
            .collect();
        assert_eq!(names.len(), 2, "expected the choice to redraw across iterations");
    }

    #[test]
    fn fails_on_invalid_dynamic_counts_and_chances() {
        let source = r#"trace "t" { repeat { count = 0 - range(2, 2) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "repeat count is negative");
        assert_eq!(&source[error.range.start..error.range.end], "0 - range(2, 2)");

        let source = r#"trace "t" { repeat { count = range(0.5, 0.5) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "repeat count is not an integer");

        let source = r#"trace "t" { maybe { chance = range(2.0, 2.0) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "maybe chance is not between 0 and 1");
    }

    #[test]
    fn plans_the_dynamic_fixture() {
        let model = compile(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();
        let plan_a = plan(model, 10, 42).unwrap();

        for range in plan_a.traces.iter() {
            let events = &plan_a.events[range.clone()];
            // root + 1..4 turns of (task + llm) + one choice pick + maybe an escalation
            assert!((4..=11).contains(&events.len()), "unexpected trace size {}", events.len());
            let picks = events
                .iter()
                .filter(|event| matches!(event.name.as_str(), "get_order_status" | "summarize_session"))
                .count();
            assert_eq!(picks, 1);
        }

        let model = compile(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();
        let plan_b = plan(model, 10, 42).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_root_bindings_once_per_trace() {
        let model = compile(
            r#"
            vars { x = range(0, 1000000) }
            trace "t" { input = var.x output = var.x }
            "#,
        )
        .unwrap();
        let plan = plan(model, 4, 7).unwrap();

        let values: Vec<JsonValue> = plan
            .events
            .iter()
            .map(|event| {
                let input = event.fields.input.clone().unwrap();
                assert_eq!(Some(&input), event.fields.output.as_ref());
                input
            })
            .collect();
        // the binding re-samples across traces
        assert!(values.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn evaluates_repeat_bindings_once_per_iteration() {
        let model = compile(
            r#"
            trace "t" {
                repeat {
                    count = 3
                    vars { r = range(0, 1000000) }
                    task "turn" { input = var.r output = var.r }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 3).unwrap();

        let turns = &plan.events[1..];
        assert_eq!(turns.len(), 3);
        let mut values = Vec::new();
        for event in turns {
            let input = event.fields.input.clone().unwrap();
            assert_eq!(Some(&input), event.fields.output.as_ref());
            values.push(input);
        }
        // the binding re-samples across iterations
        assert!(values.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn sums_span_bindings_exactly() {
        let model = compile(
            r#"
            trace "t" {
                llm "Chat Completion" {
                    vars {
                        pt = round(lognormal(600, 0.4))
                        ct = round(lognormal(90, 0.7))
                    }
                    metrics = { prompt_tokens = var.pt, completion_tokens = var.ct, tokens = var.pt + var.ct }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 5, 11).unwrap();

        let llms = plan.events.iter().filter(|event| matches!(event.kind, EventKind::Llm));
        for event in llms {
            let metrics = event.fields.metrics.as_ref().unwrap();
            let pt = metrics["prompt_tokens"].as_i64().unwrap();
            let ct = metrics["completion_tokens"].as_i64().unwrap();
            assert_eq!(metrics["tokens"].as_i64().unwrap(), pt + ct);
        }
    }

    #[test]
    fn interpolates_bindings_consistently_with_value_references() {
        let model = compile(
            r#"
            vars { m = choice("gpt-4o", "gpt-4o-mini") }
            trace "t" { input = "model: ${var.m}" metadata = { model = var.m } }
            "#,
        )
        .unwrap();
        let plan = plan(model, 6, 5).unwrap();

        for event in plan.events.iter() {
            let name = event.fields.metadata.as_ref().unwrap()["model"].as_str().unwrap().to_owned();
            assert_eq!(event.fields.input, Some(JsonValue::from(format!("model: {name}"))));
        }
    }

    #[test]
    fn shares_choice_bindings_across_alternatives() {
        let model = compile(
            r#"
            trace "t" {
                choice {
                    vars { c = range(0, 1000000) }
                    task "a" { input = var.c output = var.c }
                    task "b" { input = var.c output = var.c }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 5, 13).unwrap();

        for event in plan.events.iter().filter(|event| event.parent.is_some()) {
            assert_eq!(event.fields.input, event.fields.output);
            assert!(event.fields.input.as_ref().unwrap().is_i64());
        }
    }

    #[test]
    fn evaluates_maybe_bindings_only_on_inclusion() {
        let model = compile(
            r#"
            trace "t" {
                maybe {
                    chance = 0.5
                    vars { e = range(0, 1000000) }
                    task "escalation" { input = var.e }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 8, 17).unwrap();

        let included = plan.events.iter().filter(|event| event.parent.is_some()).count();
        assert!((1..8).contains(&included), "expected a mix of inclusions, got {included}");
    }

    #[test]
    fn threads_referenced_values_across_spans() {
        let model = compile(
            r#"
            trace "t" {
                # written above the block it reads, order is dataflow not layout
                output = llm.chat.output.content
                metadata = { echoed = "${llm.chat.output.content}" }
                task "plan" { output = "planned" }
                llm "chat" {
                    input = [{ role = "user", content = task.plan.output }]
                    output = { role = "assistant", content = choice("alpha", "beta") }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 4, 11).unwrap();

        for range in plan.traces.iter() {
            let trace = &plan.events[range.start];
            let llm = &plan.events[range.start + 2];
            let content = &llm.fields.output.as_ref().unwrap()["content"];

            // the sampled content flows to the trace output and its template
            assert_eq!(trace.fields.output.as_ref().unwrap(), content);
            let echoed = &trace.fields.metadata.as_ref().unwrap()["echoed"];
            assert_eq!(echoed, content);

            // and the task output flows into the llm input
            let input = llm.fields.input.as_ref().unwrap();
            assert_eq!(input[0]["content"], JsonValue::String("planned".to_owned()));
        }
    }

    #[test]
    fn sums_sibling_metric_keys_exactly() {
        let model = compile(
            r#"
            trace "t" {
                llm "chat" {
                    output = "brief answer"
                    metrics = {
                        prompt_tokens = round(lognormal(600, 0.4)),
                        completion_tokens = round(lognormal(90, 0.7)),
                        tokens = self.metrics.prompt_tokens + self.metrics.completion_tokens,
                        exact = tokens(self.output),
                    }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 3, 5).unwrap();

        for range in plan.traces.iter() {
            let metrics = plan.events[range.start + 1].fields.metrics.as_ref().unwrap();
            let total = metrics["prompt_tokens"].as_i64().unwrap() + metrics["completion_tokens"].as_i64().unwrap();
            assert_eq!(metrics["tokens"].as_i64().unwrap(), total);
            assert!(metrics["exact"].as_i64().unwrap() > 0);
        }
    }

    #[test]
    fn correlates_same_iteration_references_inside_repeats() {
        let model = compile(
            r#"
            trace "t" {
                repeat {
                    count = 4
                    llm "gen" { output = choice("a", "b", "c") }
                    tool "use" { input = llm.gen.output }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 2, 23).unwrap();

        let mut distinct = std::collections::HashSet::new();
        for range in plan.traces.iter() {
            // events: trace, then (llm, tool) pairs per iteration
            for pair in plan.events[range.start + 1..range.end].chunks(2) {
                let output = pair[0].fields.output.as_ref().unwrap();
                let input = pair[1].fields.input.as_ref().unwrap();
                assert_eq!(input, output, "each round's tool reads its own llm");
                distinct.insert(output.as_str().unwrap().to_owned());
            }
        }
        assert!(distinct.len() > 1, "iterations draw independently");
    }

    #[test]
    fn draws_a_referenced_field_once() {
        let model = compile(
            r#"
            trace "t" {
                task "roll" { output = range(0, 1000000) }
                task "a" { input = task.roll.output }
                task "b" { input = task.roll.output }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 3).unwrap();

        let rolled = plan.events[1].fields.output.as_ref().unwrap();
        assert_eq!(plan.events[2].fields.input.as_ref().unwrap(), rolled);
        assert_eq!(plan.events[3].fields.input.as_ref().unwrap(), rolled);
    }

    #[test]
    fn fails_generation_on_dynamic_position_cycles() {
        // the static graph skips dynamic positions, in-progress marking catches them
        let model = compile(
            r#"
            trace "t" {
                llm "x" { input = llm["x"][trace.index % 1].input output = 1 }
            }
            "#,
        )
        .unwrap();
        let error = plan(model, 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "references form a cycle");
    }

    #[test]
    fn accumulates_history_across_iterations() {
        let model = compile(
            r#"
            trace "t" {
                repeat "rounds" {
                    count = 3
                    llm "chat" {
                        input = [{ role = "user", content = trace.input }, ...repeat.rounds[:repeat.index].llm.chat.output]
                        output = { role = "assistant", content = "reply ${repeat.rounds.index}" }
                    }
                }
                input = "q"
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 7).unwrap();

        // round i replays the user turn plus i earlier assistant turns
        for (round, event) in plan.events[1..].iter().enumerate() {
            let input = event.fields.input.as_ref().unwrap().as_array().unwrap();
            assert_eq!(input.len(), 1 + round);
            for (earlier, message) in input[1..].iter().enumerate() {
                assert_eq!(message["content"], JsonValue::String(format!("reply {earlier}")));
            }
        }
    }

    #[test]
    fn reads_previous_iterations_with_guards() {
        let model = compile(
            r#"
            trace "t" {
                repeat "r" {
                    count = 3
                    task "step" {
                        output = range(0, 1000)
                        input = repeat.index > 0 ? repeat.r[repeat.index - 1].task.step.output : -1
                    }
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 13).unwrap();

        assert_eq!(plan.events[1].fields.input.as_ref().unwrap().as_i64().unwrap(), -1);
        for pair in plan.events[1..].windows(2) {
            assert_eq!(pair[1].fields.input, pair[0].fields.output);
        }
    }

    #[test]
    fn fails_generation_on_out_of_range_iterations() {
        let model = compile(
            r#"
            trace "t" {
                repeat "r" { count = 2 task "step" { output = 1 } }
                input = repeat.r[trace.index + 2].task.step.output
            }
            "#,
        )
        .unwrap();
        let error = plan(model, 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "array index is out of bounds");
    }

    #[test]
    fn reports_branch_outcomes_and_nulls() {
        let model = compile(
            r#"
            trace "t" {
                choice "outcome" {
                    task "resolved" { output = "ok" }
                    task "escalated" { output = "paged" }
                }
                maybe "retry" { chance = 0.5 task "again" { output = 1 } }
                metadata = {
                    picked = choice.outcome.chosen,
                    resolved = choice.outcome.task.resolved.output != null,
                    retried = maybe.retry.included,
                }
            }
            "#,
        )
        .unwrap();
        let plan = plan(model, 16, 29).unwrap();

        let mut picks = std::collections::HashSet::new();
        let mut retries = std::collections::HashSet::new();
        for range in plan.traces.iter() {
            let trace = &plan.events[range.start];
            let metadata = trace.fields.metadata.as_ref().unwrap();
            let picked = metadata["picked"].as_i64().unwrap();
            picks.insert(picked);
            retries.insert(metadata["retried"].as_bool().unwrap());

            // the null probe agrees with the recorded pick
            assert_eq!(metadata["resolved"].as_bool().unwrap(), picked == 0);

            // the picked branch's span is the one that emitted
            let branch = &plan.events[range.start + 1];
            let expected = if picked == 0 { "resolved" } else { "escalated" };
            assert_eq!(branch.name, expected);
        }
        assert_eq!(picks.len(), 2, "both branches occur across traces");
        assert_eq!(retries.len(), 2, "both maybe outcomes occur across traces");
    }

    #[test]
    fn keeps_unrelated_draws_stable_across_edits() {
        // path-addressed rngs: editing one field must not reshuffle another's draws
        let before = compile(r#"trace "t" { input = range(0, 1000000) output = "x" }"#).unwrap();
        let after = compile(r#"trace "t" { input = range(0, 1000000) output = "y ${trace.index}" }"#).unwrap();

        let before = plan(before, 5, 99).unwrap();
        let after = plan(after, 5, 99).unwrap();

        for (before, after) in before.events.iter().zip(after.events.iter()) {
            assert_eq!(before.fields.input, after.fields.input);
        }
    }
}
