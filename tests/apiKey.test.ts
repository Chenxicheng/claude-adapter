import { parseApiKeyEnvInput } from '../src/utils/apiKey';

describe('parseApiKeyEnvInput', () => {
  it('returns a configured portable environment variable name', () => {
    expect(parseApiKeyEnvInput(' OPENAI_API_KEY ', { OPENAI_API_KEY: 'secret' })).toBe(
      'OPENAI_API_KEY'
    );
  });

  it.each(['', '1OPENAI_KEY', 'OPENAI-KEY', 'OPENAI.KEY'])('rejects invalid name %p', (name) => {
    expect(() => parseApiKeyEnvInput(name, {})).toThrow(
      'Environment variable name must match [A-Za-z_][A-Za-z0-9_]*'
    );
  });

  it('rejects a missing environment variable', () => {
    expect(() => parseApiKeyEnvInput('OPENAI_API_KEY', {})).toThrow(
      'Environment variable "OPENAI_API_KEY" is not set'
    );
  });

  it('rejects an empty environment variable', () => {
    expect(() => parseApiKeyEnvInput('OPENAI_API_KEY', { OPENAI_API_KEY: '' })).toThrow(
      'Environment variable "OPENAI_API_KEY" is empty'
    );
  });
});
