import React from 'react'
import ReactDOM from 'react-dom/client'
import {installPreviewRuntime} from './runtime'
import '../../../src/styles.css'
import '../../../src/navigation.css'

// Only this separate browser entry installs the synthetic native boundary.
// The desktop entry and its owned runtime remain unchanged.
installPreviewRuntime()
void import('../../../src/App').then(({default: App}) => {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode><App initialThreadId="preview-writing" /></React.StrictMode>,
  )
}).catch(() => {
  document.getElementById('root')!.textContent = 'The app preview could not load. Refresh this page to try again.'
})
