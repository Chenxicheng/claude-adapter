export function isOpenAIOSeriesModel(model: string): boolean {
  const normalized = model.trim().toLowerCase();
  return normalized.length > 1 && normalized.startsWith('o') && /\d/.test(normalized[1]);
}

export function isOpenAIReasoningModel(model: string): boolean {
  const normalized = model.trim().toLowerCase();
  return isOpenAIOSeriesModel(normalized) || normalized.startsWith('gpt-5');
}

export function isGlm5Model(model: string): boolean {
  return model.trim().toLowerCase().startsWith('glm-5');
}

export function isGlm52Model(model: string): boolean {
  return model.trim().toLowerCase().startsWith('glm-5.2');
}

export function isQwen3Model(model: string): boolean {
  return model.trim().toLowerCase().startsWith('qwen3');
}
