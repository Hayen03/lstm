# v3

- [x] Identify LSTM symbols by Unicode extended grapheme clusters instead of individual Unicode code points, allowing a single symbol to contain multiple code points (for example, a letter with a combining accent or an emoji sequence). Apply this consistently to declarations, words, rule matching, and context patterns; update the syntax documentation and add regression coverage.
- [ ] Custom `.lstm` file format to become independent from json5 (old json5 files will still be supported)
- [x] Dynamic growing and drawing rules that allow splicing the `to` field with `${...}` formulas that evaluates to a list of tokens `[{symbol: str, args: dict}...]`
- [x] Change the origin to be at the bottom-center of the canvas (or a bit above)
- [x] Rajouter les arguments `color` au symboles `F`, `D` et `L`, qui indique la couleur de dessin pour cette instruction sans changer la couleur de base du crayon.
- [x] Add a `W` instruction that changes the default width of the brush? Also `width()` and `color()` to formulas to allow access to the width and color of the brush.

# Future

- [x] Normalize symbols to Unicode NFC so canonically equivalent spellings match in declarations, rules, contexts, and trace symbol filters. Preserve case and compatibility distinctions.
- [x] configurable recursion limit.
- [ ] Add support for unicode escape seq in words. A word such as `E\u0301{x:5}` should be parsed as one symbol `é{x:5}` instead of 2.
- [ ] Allow formulas in field that would ordinarily use literals in the native file format (such as `name`, `weight`, etc.)
