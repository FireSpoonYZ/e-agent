"""Pure Python test tools."""


def multiply(x, y):
    return x * y


__e_agent_extension__ = {
    "name": "pure_tools",
    "description": "Pure Python test tools.",
    "systemPrompt": "Use pure_tools.multiply for exact integer products.",
    "functions": [
        {
            "name": "multiply",
            "description": "Multiply two numbers",
            "requires_await": False,
            "schema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "x": {"type": "integer", "description": "first factor"},
                    "y": {"type": "integer", "description": "second factor"},
                },
                "required": ["x", "y"],
            },
            "output_schema": {"type": "integer"},
        }
    ],
}
