# MCP server

`deckcraft-cli mcp` runs a Model Context Protocol server on stdio. By default it drives its own
headless engine (no window); with `--connect PORT` it drives a running DeckCraft started with
`--control PORT`, so an agent and a person can work on the same deck. `--sample` opens the sample
deck.

```json
{
  "mcpServers": {
    "deckcraft": { "command": "deckcraft-cli", "args": ["mcp", "--connect", "7990"] }
  }
}
```

## Tools

| Tool | What it does |
|---|---|
| `list_commands` | Every command (id, label, menu, shortcut, params doc), optionally filtered |
| `run_command` | Run one command: `{command, params}` — every menu item, ribbon button and gesture is a command |
| `batch` | Run several commands in order and stop at the first error |
| `new_presentation`, `open_presentation`, `save_presentation`, `export` | Files: native `.deckcraft`, PowerPoint `.pptx`, PDF (slides, notes pages, handouts), PNG/JPEG, outline |
| `inspect_document`, `inspect_slide` | Structure of the deck / one slide (shapes, text, placeholders, animations) |
| `render_slide` | A slide as an image the model can look at |
| `add_slide`, `go_to_slide` | Slides by layout |
| `add_shape`, `add_text_box`, `set_text` | Shapes and text |
| `insert_picture`, `insert_table`, `insert_chart` | Content |
| `select`, `pointer`, `select_tool`, `key`, `type_text` | The same selection, mouse and keyboard paths as the UI |
| `screenshot` | The whole window, UI included (connected mode) |
| `call_app` | Any [control protocol](control-protocol.md) method (connected mode) |

Useful commands for building decks: `slide.new {layout, title, body}`, `shape.insert {preset,
rect, text}`, `shape.connect {from, to, preset}`, `shape.freeform {points, closed, smooth}`,
`shape.merge {op, ids}`, `format.*`, `design.theme`, `transition.set`, `animation.set`,
`chart.*`, `table.*`, `file.export {path, layout}`, `template.fields`, `template.fill {data}`.
`deckcraft-cli describe ID` prints any command's parameters.

## Templates and data

A deck can be a template: write `{{name}}` where a value goes (titles, body text, table cells,
grouped shapes, notes). `template.fields` lists the names a deck uses, and `template.fill {data:
{name: value}}` replaces every token, keeping the formatting of the text it sits in. Tokens that
PowerPoint split across several runs are still found. Names without a value are left in place
and reported (`missing: "empty"` blanks them, `missing: "error"` refuses to change anything).

From the CLI, `--data` takes the values from a JSON file and runs before any `--cmd`:

```sh
deckcraft-cli run --in quarterly.pptx --data fields.json --cmd 'file.export={"path":"q3.png","slide":0}'
deckcraft-cli run --in quarterly.pptx --cmd 'template.fields' --print
```
