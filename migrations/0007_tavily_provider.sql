-- Optional Tavily search credential reference. Search enable/config stays in params_json.
ALTER TABLE llm_providers ADD COLUMN tavily_key_ref TEXT;
