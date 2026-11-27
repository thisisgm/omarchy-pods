import {gi} from './gnome_stubs.js';

export default gi[new URL(import.meta.url).search.slice(1)];
