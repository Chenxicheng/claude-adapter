function normalizeModelName(model: string): string {
  return model.trim().toLowerCase();
}

export function isOpenAIOSeriesModel(model: string): boolean {
  const normalized = normalizeModelName(model);
  return normalized.length > 1 && normalized.startsWith('o') && /\d/.test(normalized[1]);
}

export function isOpenAIGpt5Model(model: string): boolean {
  return normalizeModelName(model).startsWith('gpt-5');
}

export function isOpenAIReasoningModel(model: string): boolean {
  return isOpenAIOSeriesModel(model) || isOpenAIGpt5Model(model);
}

export function isGlm5Model(model: string): boolean {
  return normalizeModelName(model).startsWith('glm-5');
}

export function isGlm52Model(model: string): boolean {
  return normalizeModelName(model).startsWith('glm-5.2');
}

export function isQwen3Model(model: string): boolean {
  return normalizeModelName(model).startsWith('qwen3');
}
