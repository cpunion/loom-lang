# Syntax emission

`emit(node, fragment)` converts a public `std.loom.ast.Node` into source for a
`Fragment.File`, `Expression`, `Type`, `Statement`, `Declaration`, `Pattern`, or
`Binding`. `expression(node)` is the expression shortcut. Both return
`Result[Text, Text]`.

The emitter uses the ordinary grammar to reparse its output and verifies the
same tree, ignoring byte spans. Missing children, source smuggled through a name,
internal compiler markers, cycles and excessive expansion reject. Trees may be
assembled directly or obtained from the public parser and transformed. Names,
types, contracts and visibility still need normal binding and checking.

This is structural emission, not a lossless file editor: ASTs have no comments
or whitespace. Spans identify the input revision and are not copied into output.
Use `std.loom.format.format` to format a whole emitted file. See the
[declaration generation tool](../../../examples/ast_generation) and
[AST macro example](../../../examples/typed_macros/trees.loom).
