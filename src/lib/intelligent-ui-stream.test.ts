import { describe, expect, it } from 'vitest'
import { previewUiDocument } from './intelligent-ui-stream'

describe('progressive Intelligent UI compilation', () => {
  it('reveals only complete components without repairing unfinished strings or executing content', () => {
    const source = '{"version":1,"kind":"native","title":"Compare","text":"Choices","initialState":{},"actions":[],"components":[{"id":"a","type":"text","text":"Ready"},{"id":"b","type":"text","text":"Unfinished'
    expect(previewUiDocument(source)).toMatchObject({title: 'Compare', components: [{id: 'a', type: 'text', text: 'Ready'}]})
    expect(previewUiDocument('{"title":"a string containing \\"components\\": []"')).toBeNull()
    expect(previewUiDocument(source.replace('"version":1', '"version":99'))).toBeNull()
  })
})
