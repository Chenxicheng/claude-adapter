const RESERVED_HEADERS = new Set([
  'authorization',
  'content-type',
  'accept',
  'content-length',
  'host',
]);

export function parseUpstreamHeadersInput(input: string): Record<string, string> | undefined {
  const trimmed = input.trim();
  if (!trimmed) {
    return undefined;
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    throw new Error('Please enter a valid JSON object');
  }

  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('Headers must be a JSON object');
  }

  const headers: Record<string, string> = {};

  for (const [name, value] of Object.entries(parsed)) {
    const headerName = name.trim();
    if (!headerName) {
      throw new Error('Header names must be non-empty strings');
    }
    if (RESERVED_HEADERS.has(headerName.toLowerCase())) {
      throw new Error(`Header "${headerName}" is reserved and cannot be configured`);
    }
    if (typeof value !== 'string' || value.trim() === '') {
      throw new Error(`Header "${headerName}" value must be a non-empty string`);
    }

    headers[headerName] = value;
  }

  return Object.keys(headers).length > 0 ? headers : undefined;
}
