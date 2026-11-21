# SpacelizardDMM

`spacelizard` is a static contract and variable-mutation checker for BYOND DM
projects. It runs against a project's DME using DreamMaker syntax from
SpacemanDMM's `dreammaker` parser.

## Build and run

Build with `cargo build --release --locked`. The binary is `spacelizard`
(`spacelizard.exe` on Windows). The project pins Rust 1.95 because
`dreammaker` in SpacemanDMM `suite-1.12` requires it. The parser tag matches
BeeStation's DreamChecker. BeeStation's Rust 1.89 pin controls its rust-g build
and can remain at 1.89 because BeeStation consumes a prebuilt Spacelizard
binary.

From a consumer project directory:

```sh
spacelizard sync-header code/__DEFINES/spacelizard.dm
spacelizard check --verify-header
spacelizard check --ci --format=github --timings
```

Include the synchronized header from the DME before your rule declarations and
proc markers. The checker injects `SPACELIZARD` in memory. In normal BYOND
compilation the declarations disappear and the expression wrappers yield their
argument.

```toml
environment = "project.dme"
header_path = "code/__DEFINES/spacelizard.dm"

[passes]
default = ["CBT"]
ci_extra = [["CBT", "CIBUILDING"]]

[rules]
var_alias_escape = "warning"
```

Paths resolve relative to `spacelizard.toml` (or the file passed with
`--config`). Passes are independent define sets. `--ci` runs the default and
all extra passes concurrently. `-D NAME[=VALUE]` adds a define to each pass.
`[[forwarding]]` is reserved. Rule severities are `error`, `warning` and
`off`. The checker exits 1 on errors. Output is text
(`file:line:col: error[rule_id]: message`), GitHub annotations, or JSON.
`--timings` reports preprocessing and parsing, analysis, output and total
wall time on stderr.

## Proc contracts

```dm
/datum/example/proc/configure(name, enabled = TRUE)
	SHOULD_CALL_REQUIRED_PARAMS(TRUE)
	SHOULD_STATE_PARAM_NAMES(TRUE)
	SHOULD_KEEP_PARAM_ORDER(TRUE)
```

- `SHOULD_CALL_ALL_PARAMS` requires every parameter and rejects defaults on
  the definition. `SHOULD_CALL_REQUIRED_PARAMS` permits omitted parameters
  that have defaults.
- `SHOULD_STATE_PARAM_NAMES` requires keyword calls.
  `SHOULD_KEEP_PARAM_ORDER` requires bound parameters in declaration order.
- Descendants inherit each flag. An override can disable a flag with `FALSE`.
- `PARAM_LINT_SKIP(expression)` exempts the outermost violating call or
  reference in its argument. Nested calls are still checked.
- Diagnostic IDs include `missing_params`, `too_many_args`, `positional_arg`,
  `param_order`, `positional_after_keyword`, `duplicate_binding`,
  `unknown_keyword`, `arglist`, `unverifiable_call`, `proc_reference`,
  `dynamic_call`, `override_params`, `variadic`, `marked_verb`,
  `default_value`, `bad_marker`, `unused_skip`, and `unparsable_proc`.

## Variable rules

```dm
SET_VAR_RULE(/client/images, ASSIGN_NOT_ALLOWED | LIST_MUTATE_NOT_ALLOWED,
	allow_in = list(/proc/refresh_images), hint = "Use the image helper.")

/client/proc/add_overlay_image(image/I)
	src.images += BYPASS_VAR_RULE(I)

/proc/reset_images(client/C)
	BYPASS_VAR_RULE_BLOCK
		C.images.Cut()
		C.images += new /image()
```

A type block can declare rules by relative variable name. `ASSIGN_NOT_ALLOWED`
covers direct assignment, compound assignment and increments. With
`LIST_MUTATE_NOT_ALLOWED` the checker also rejects index writes and the
`Add`, `Remove`, `Cut`, `Insert`, `Swap` and `Splice` methods. Rules
apply to subtypes. `allow_in` accepts types (including their descendants) or
exact proc paths. `ALLOW_IN_CONSTRUCTOR` permits writes from `New` and
`Initialize` on the declaring type and subtypes.

`BYPASS_VAR_RULE` can wrap the mutation or the RHS of its assignment.
An unused bypass emits `unused_var_bypass`. Local list aliases of protected
fields that are mutated (`+=`, `-=`, `|=`, `&=`, mutating list methods, or
index writes) produce `var_alias_escape`; an unknown or supertype receiver
using dynamic `:` access can produce `unverifiable_var_mutation`. Other
variable IDs include `var_assign_forbidden`, `var_list_mutate_forbidden`
and `bad_var_rule`.

Type inference is local and approximate. Arbitrary dynamic dispatch, lifecycle
forwarding, control-flow narrowing and aliases across procedures remain
outside this release. `dm_parse` reports parser warnings/errors and inherits
their severity from the parser.

## License

GPL-3.0-or-later. The binary links the GPL-licensed SpacemanDMM `dreammaker`
from its git repository, along with its path dependencies. See `LICENSE`
for the full GPL version 3 text.
