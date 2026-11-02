import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import App from './App'

vi.mock('./lib/client', () => ({isDesktop:()=>true, client:{
  subscribe:async()=>()=>{},
  connect:async()=>({account:null,models:[],threads:[]}),
}}))
vi.mock('./components/ExtensionsPanel', () => ({ExtensionsPanel:({onClose}:{onClose:()=>void})=><section aria-label="Extensions settings"><button onClick={onClose}>Close extensions</button></section>}))

it('opens extension management directly from the desktop sidebar and returns to the conversation', async () => {
  render(<App />)
  await userEvent.click(screen.getByRole('button', {name:'Plugins'}))
  await userEvent.click(screen.getByRole('button', {name:'Plugins'}))
  expect(screen.getByRole('region', {name:'Extensions settings'})).toBeInTheDocument()
  await userEvent.click(screen.getByRole('button', {name:'Close extensions'}))
  expect(screen.queryByRole('region', {name:'Extensions settings'})).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', {name:'Message'})).toBeInTheDocument()
})
