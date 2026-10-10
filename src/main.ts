import { createApp } from "vue";
import App from "./App.vue";
import "./styles/tokens.css";
import { applyTheme, currentTheme } from "./lib/theme";

applyTheme(currentTheme());

createApp(App).mount("#app");
