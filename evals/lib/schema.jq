# questions.json -> the JSON Schema an agent's reply must satisfy.
def answer_type:
  if .kind == "command" or .kind == "text" or .kind == "tokens" then {type: "string"}
  elif .kind == "bool" then {type: "boolean"}
  elif .kind == "set" or .kind == "list" then {type: "array", items: {type: "string"}}
  elif .kind == "enum" then {type: "string", enum: .values}
  else error("unknown kind \(.kind) on \(.id)")
  end;

{
  type: "object",
  additionalProperties: false,
  required: ["answers", "confidence", "unclear"],
  properties: {
    answers: {
      type: "object",
      additionalProperties: false,
      required: map(.id),
      properties: (map({
        key: .id,
        value: {
          type: "object",
          additionalProperties: false,
          required: ["answer", "stated"],
          properties: {answer: answer_type, stated: {type: "boolean"}}
        }
      }) | from_entries)
    },
    confidence: {type: "integer", minimum: 1, maximum: 5},
    unclear: {type: "array", items: {type: "string"}}
  }
}
