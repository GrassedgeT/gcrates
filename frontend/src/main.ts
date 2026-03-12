import { mount } from 'svelte'
import './global.css'
import GlobalGraphApp from './GlobalGraphApp.svelte'

const app = mount(GlobalGraphApp, {
  target: document.getElementById('app')!,
})

export default app
