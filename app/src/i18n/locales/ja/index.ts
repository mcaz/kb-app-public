import common from "./common.json";
import connect from "./connect.json";
import files from "./files.json";
import graph from "./graph.json";
import github from "./github.json";
import home from "./home.json";
import notes from "./notes.json";
import onboarding from "./onboarding.json";

/** 日本語が文言の正本(NFR-5: 日本語第一級)。キーの型もここから引く。 */
export const ja = { common, notes, files, home, graph, connect, onboarding, github };
export default ja;
