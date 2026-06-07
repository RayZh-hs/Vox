# MIR

MIR is the middle intermediate representation used between semantic analysis and
backend lowering.

It is source-facing enough to inspect, but executable enough for optimization.
Source names are resolved before MIR. Rebindings are split into explicit binding
versions. Runtime values are represented by SSA-style value ids. Lifetimes,
escapes, materialization demand, copy-on-write eligibility, and slot reuse are
computed on MIR.

## Module

A MIR module contains executable bodies for:

- the script entry body;
- package value initializers;
- top-level functions;
- lambda bodies.

```text
MirModule
  module: ModulePath
  kind: script | evil script | package
  optimization: NOpt | IOpt | SOpt
  bodies: [MirBody]
```

## Body

Each body is a control-flow graph.

```text
MirBody
  id: BodyId
  name: String
  kind: script_entry | value_initializer | function | lambda
  purity: pure | evil
  rank: baseline | interactive | sealed-ownership | sealed-demand | sealed-materialization
  params: [ValueId]
  captures: [Capture]
  bindings: [Binding]
  values: [Value]
  blocks: [Block]
  analyses: AnalysisSummary
```

Blocks use block parameters as phi nodes. A predecessor passes the values needed
by the successor through `jump` or `branch`.

```text
Block
  id: BlockId
  params: [ValueId]
  ops: [Op]
  term: Terminator
```

## Binding Nodes

`BindingId` identifies a source declaration. Two declarations with the same
source name always have different ids.

```text
Binding
  id: BindingId
  name: String
  mutability: val | var
  scope_depth: u32
  declared_type: Type?
  span: TextSpan
  capture: local | captured | noncapturable
  versions: [VersionId]
```

`VersionId` identifies the value currently held by a binding at a point in the
body. A `val` usually has one version. A `var` has one version for its
initializer and one for each assignment.

```text
Version
  id: VersionId
  binding: BindingId
  value: ValueId
  source: initializer | assignment | compound_assignment | join | loop
```

The executable graph uses `ValueId` operands, not source names. Bindings remain
for inspection, diagnostics, and optimization explanations.

## Value Nodes

```text
Value
  id: ValueId
  type: Type?
  def: parameter | capture | block_param | op | literal | unit
  binding_version: VersionId?
  uses: [Use]
  lifetime: Lifetime
  escape: Escape
  demand: Demand
  storage: Storage
```

`Lifetime` records first definition, last use, live-in blocks, live-out blocks,
and whether storage can be reused after the last use.

`Escape` records whether the value is returned, captured, stored in `econ`,
passed to an evil call, or passed to an unknown host boundary.

`Demand` records whether the full value is needed or only selected tuple slots
or record fields are demanded.

`Storage` records the current slot assignment and copy-on-write status:

- `fresh`: new storage is required;
- `reuse(ValueId)`: the value can reuse storage from an ended lifetime;
- `cow(ValueId)`: the value can share storage until a write forces a copy;
- `virtual`: no materialized storage is required.

## Operation Nodes

```text
Op
  result: ValueId?
  kind: OpKind
  args: [ValueId]
  span: TextSpan?
```

Operation kinds:

- `literal(value)`;
- `unary(op)`;
- `binary(op)`;
- `tuple(shape)`;
- `record(shape)`;
- `list`;
- `project(field | slot)`;
- `index`;
- `updated(path)`;
- `call(callee, purity)`;
- `econ(type)`;
- `non_null`;
- `safe_project(field)`;
- `type_test(type)`;
- `type_refine(type)`;
- `iterator`;
- `iterator_next`;
- `cache_get(key)`;
- `cache_put(key)`.

Short-circuit `&&`, `||`, and `?:` lower to branches and joins unless a prior
analysis proves eager evaluation is equivalent.

## Terminator Nodes

```text
Terminator
  jump(target, args)
  branch(condition, then_target, then_args, else_target, else_args)
  return(value)
  panic(message)
  unreachable
```

`if`, `when`, loops, early `return`, and `panic` are represented with blocks and
terminators. Branch result values flow through join block parameters.

## Lowering Semantics

The lowering environment maps each visible source name to its current binding
version.

Declaration lowering:

1. Lower the initializer.
2. Create a new `BindingId`.
3. Create version zero for the binding.
4. Map the source name to that version in the current lexical scope.

Assignment lowering:

1. Resolve the target name to a mutable binding.
2. Lower the right-hand expression.
3. Create a new version for the same binding.
4. Update the environment so later references use the new version.

Compound assignment lowering:

1. Resolve the target name to the current value.
2. Lower the right-hand expression.
3. Emit the matching `binary` operation.
4. Create a new version for the target binding.

Shadowing lowering creates a new `BindingId` and restores the previous name
mapping when the lexical scope exits.

Branch joins compare the binding versions leaving each branch. If a binding has
different outgoing versions, the join block receives a block parameter and the
environment maps that binding to a join-created version.

Loops use loop header block parameters for loop-carried mutable bindings. The
loop pattern is a fresh binding in the loop body.

`when` lowers the subject once, then emits type tests. An `as` binding is a
fresh binding whose value is the refined subject inside that arm.

## Text Format

MIR text is an inspection format, not the executable storage format. It keeps
the surface close to Vox while exposing compiler facts.

Example source:

```vox
var x = 1;
x = x + 2;
{
    val x = "local";
    x
}
x
```

Example MIR text:

```text
body @script_entry pure rank=interactive {
  binding %b0 var x scope=0 versions=[%v0,%v1]
  binding %b1 val x scope=1 versions=[%v2]

  block %bb0:
    %0 = literal 1
    bind %v0 -> %b0 = %0
    %1 = use %v0
    %2 = literal 2
    %3 = binary add %1, %2
    bind %v1 -> %b0 = %3 lifetime(%0..%3, reusable)
    %4 = literal "local"
    bind %v2 -> %b1 = %4 lifetime(%4..%5, reusable)
    %5 = use %v2
    drop %5
    %6 = use %v1 lifetime(%3..return, escapes=return)
    return %6
}
```

The text format includes:

- body kind, purity, and optimization rank;
- binding declarations and version lists;
- block labels;
- value-producing operations;
- bind statements showing `VersionId` creation;
- lifetime summaries;
- escape and demand summaries when non-default;
- storage summaries when slot reuse, copy-on-write, or virtualization applies.

## Optimization Passes

Optimization is extensible through ordered MIR passes. A pass receives a mutable
body plus analysis facts and returns whether it changed the body.

Required built-in passes:

- control-flow cleanup;
- def-use construction;
- lifetime analysis;
- active value caching for `IOpt`;
- demand analysis for tuple and record projections;
- function result culling for unused tuple slots and record fields;
- copy-on-write marking;
- storage slot reuse;
- sealed compaction for `SOpt`.

Custom pass groups can be installed around the built-in groups:

- before cleanup;
- after analysis;
- before sealed compaction;
- before backend lowering.

Custom passes must preserve Vox behavior. Passes that move, remove, or share
operations must respect evil call ordering and escape facts.

## Backend Contract

MIR does not encode wasm layout.

The wasm backend chooses local allocation, stack use, memory layout, imported
function ABI, runtime helper calls, and final instruction order. MIR supplies
typed values, explicit control flow, effect metadata, lifetimes, demand, and
storage reuse facts.
