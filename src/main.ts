import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import "./styles/tokens.css";
import { applyTheme, currentTheme } from "./lib/theme";

applyTheme(currentTheme());

const app = createApp(App);
app.use(createPinia());
app.mount("#app");
