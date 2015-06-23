import {component} from 'util/base'
import {Navbar} from 'util/navbar'
import {Context} from 'util/base'

import {Processes} from 'pages/processes'
import {Status} from 'pages/status'
import {Values} from 'pages/values'
import {Totals} from 'pages/totals'
import {Metrics} from 'pages/metrics'
import {update, append} from 'util/render'


export class App {
    constructor() {
    }
    render() {
        return {tag: 'div', children: [
            component(Navbar),
            this.page ? component(this.page) : "",
            ]}
    }
    static start() {
        var app = new App();
        window.onhashchange = function() {
            if(app.page) {
                app.page = null
            }
            if(window.location.hash == '#/processes') {
                app.page = Processes;
            } else if(window.location.hash == '#/status') {
                app.page = Status;
            } else if(window.location.hash == '#/values') {
                app.page = Values;
            } else if(window.location.hash == '#/totals') {
                app.page = Totals;
            } else if(window.location.hash == '#/metrics') {
                app.page = Metrics;
            }
            update()
        }
        append(document.body, app.render.bind(app))
        window.onhashchange()
    }
}

App.start()
