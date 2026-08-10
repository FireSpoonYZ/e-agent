"""Pure Python test tools."""


async def multiply(x, y):
    return x * y


__e_agent_tools__ = [
    {
        "name": "multiply",
        "description": "Multiply two numbers",
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
]
