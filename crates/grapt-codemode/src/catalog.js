// Pure metadata queries over this execution's already validated catalog.
// No host call, activation, permission decision or effect occurs here.
((definitions) => {
  const stringify = JSON.stringify;
  const parse = JSON.parse;
  const copy = (value) => parse(stringify(value));
  const byName = Object.create(null);
  for (const tool of definitions) byName[tool.name] = tool;
  // Search the same deferred metadata used by describe, without returning schemas
  // until requested.
// This bounded catalog uses matched-term weights rather than a corpus index.
  const entries = definitions.map((tool, index) => ({
    tool, index, name: tool.name.toLowerCase(),
    description: tool.description.toLowerCase(),
    schema: stringify(tool.input_schema).toLowerCase()
  }));
  return Object.freeze({
    search(query = "", limit = 8) {
      if (typeof query !== "string" || query.length > 256 ||
          !Number.isInteger(limit) || limit < 1 || limit > 16) {
        throw new Error("catalog.search expects a query up to 256 characters and a limit of 1..16");
      }
      const words = [...new Set(query.toLowerCase().match(/[\p{L}\p{N}]+/gu) || [])];
      const matches = entries.map((entry) => ({
        ...entry,
        score: words.reduce((score, word) => score +
          (entry.name.includes(word) ? 8 : 0) +
          (entry.description.includes(word) ? 4 : 0) +
          (entry.schema.includes(word) ? 1 : 0), 0)
      })).filter((entry) => words.length === 0 || entry.score > 0)
        .sort((a, b) => b.score - a.score || a.index - b.index);
      return {
        matches: matches.slice(0, limit).map(({tool}) => ({
          name: tool.name, description: Array.from(tool.description).slice(0, 240).join("")
        })),
        total: matches.length,
        truncated: matches.length > limit
      };
    },
    describe(name) {
      if (typeof name !== "string" || name.length > 256 || !byName[name]) {
        throw new Error("catalog.describe requires an available tool name");
      }
      return copy(byName[name]);
    }
  });
})
