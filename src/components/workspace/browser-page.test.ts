import { describe, expect, it, vi } from 'vitest'
import pageScript from '../../../src-tauri/src/workspace-browser/page.js?raw'

describe('native browser page helper', () => {
  it('registers before the document element exists', () => {
    const page = {}
    const doc = {addEventListener: vi.fn(), createElement: () => document.createElement('style'), head: null, documentElement: null}
    expect(() => new Function('window', 'document', pageScript)(page, doc)).not.toThrow()
    expect(page).toHaveProperty('__brrBrowser')
  })

  it('returns structured snapshots, collects annotations and operates page controls', () => {
    document.body.innerHTML = '<h1 id="heading">Example page</h1><form><input aria-label="Search"><button>Search</button></form>'
    const bounds = vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({width: 100, height: 20, x: 0, y: 0, top: 0, left: 0, right: 100, bottom: 20, toJSON: () => ({})})
    const page: Record<string, any> = {}
    new Function('window', 'document', 'location', pageScript)(page, document, window.location)
    const helper = page.__brrBrowser
    const snapshot = helper.snapshot()
    expect(snapshot).toMatchObject({title: document.title, refs: expect.any(Array)})
    const field = snapshot.refs.find((entry: {label: string}) => entry.label === 'Search')
    helper.action({action: 'fill', ref: field.id, text: 'my query'})
    expect(document.querySelector('input')).toHaveValue('my query')
    vi.stubGlobal('CSS', {escape: (value: string) => value, supports: () => true})
    helper.annotate(true, '#ff66bb')
    document.querySelector('h1')!.dispatchEvent(new MouseEvent('click', {bubbles: true, cancelable: true}))
    expect(helper.snapshot(true).selection).toMatchObject({selector: '#heading', quote: 'Example page'})
    expect(helper.snapshot(true).selection).toBeUndefined()
    bounds.mockRestore()
    vi.unstubAllGlobals()
  })
})
